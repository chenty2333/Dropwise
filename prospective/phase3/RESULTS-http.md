# Phase 3 HTTP 客户端结果（5 个目标）

> 2026-10-19。冻结的计划与目标清单见同目录 `PLAN.md`、`targets.md`（sha256 与 `FREEZE.txt` 一致，未改动）；本文所有偏离见 `AMENDMENTS.md` 第 8–13 条。
> 范围：8 个冻结目标中分给本次会话的 5 个 HTTP 客户端（hyper-util、reqwest、pingora、isahc、awc）。数据库目标由另一个会话负责，结果不在本文。
> 运行入口：`REPS=3 bash prospective/phase3/run-http.sh`；代码在 `http-clients/`。`release` = crates.io 上的原版；`fixed` = 打补丁的副本（本次未运行，原因见 §8）。

## 0. 摘要

- **没有确认的问题。** 5 个目标 × 2 种请求形状 × 4 个层级（另加 M1-residue）× 3 轮，release 版共 1464 个计划（含 M1-residue 诊断计划），违反 **0** 个；Q1 只计 B1–B3/M，M1-residue 只用于 Q3。另有一份 buggy 客户端阳性对照，证明这套观察确实能抓到错位回复（§6）。
- **B1、B2、B3、M 全部为 0。** 因此没有“M1 抓到、B3 没抓到”的计划，逐计划比较集合为空。
- **M1-residue 在 5/5 个目标上可用，残留字节在所有计划里都是 0**（阳性对照为 262252 字节）。Q3 没有额外发现；不能区分“客户端读掉了残留”和“客户端关闭了连接”，但两种情况下幸存者都拿到了自己的回复。
- 5 个客户端在未取消的普通请求上都会复用同一条连接（`LADDER-REUSE`，§6）；所以 0 不是“池根本没有复用”造成的。
- 总工时约 75 分钟（含共享环境），每个目标都远低于半天预算。成本见 §5。

## 1. 测了什么

### 1.1 目标与版本

| # | 目标（合并的 crate） | 本实验用的版本 | 复用单元 |
|---:|---|---|---|
| 1 | hyper-util 旧版 `Client`（合并 hyper、clickhouse） | hyper-util 0.1.21，hyper 1.12.0 | `hyper_util::client::legacy::Client<HttpConnector, Full<Bytes>>`，`pool_max_idle_per_host(1)`；HTTP/1.1 keep-alive 连接 |
| 2 | reqwest | reqwest 0.13.5（`default-features = false`, `charset`） | reqwest 自己的池，`http1_only()`，`pool_max_idle_per_host(1)` |
| 4 | pingora-core 的 HTTP/1 客户端连接 + pingora-pool（合并 pingora） | pingora-core 0.9.0、pingora-pool 0.9.0、pingora-http 0.9.0 | `connectors::http::v1::Connector`（`ConnectorOptions::new(1)`，内部是 pingora-pool）→ `HttpSession` → `release_http_session` |
| 6 | isahc（libcurl 的连接复用） | isahc 2.0.1，静态 curl 8.22.0 | libcurl 连接缓存；`max_connections_per_host(1)`，`connection_cache_size(1)` |
| 7 | awc（actix 的 HTTP 客户端连接池） | awc 3.10.0（`default-features = false`） | awc 默认 `Client`/connector/pool；HTTP/1.1 |

共同依赖：tokio 1.53.3；Dropwise 本身用仓库当前工作树（`dropwise = { path = "../../.." }`）。clickhouse 只是属于第 1 个目标的 HTTP 包装（`targets.md` 的合并规则），没有单独跑；`pingora` 元 crate 也没有单独跑，用的是它实际暴露的 pingora-core/pingora-pool。

### 1.2 选的操作

这 5 个客户端都没有“建立会话状态”的用户可见调用：连接池的 checkout、TCP 连接、keep-alive 都是请求内部的实现细节，没有 `begin`/`connect`/`handshake` 式的公开 API。因此按 `docs/choosing-targets.md` 的清单，每个目标只选一个目标操作：

> **一个完整的普通请求**：发出请求、读到响应头、读完整个响应体，然后（pingora）把 session 还回池。取消就落在这个操作任一次 `Pending` 上。

选它的理由：这是这些客户端唯一的用户可见的一问一答单元；HTTP/1.1 的复用单元正是一条按顺序一问一答的字节流。没有第二个操作可选，也没有测连接池内部 `checkout` 的公开入口。

**两种请求形状**（同一个操作的两个输入，见 `AMENDMENTS.md` 第 9 条）：

- **L1**：小的 `GET /a`，响应体 256 KiB。取消只能落在“等响应头/读响应体”处。
- **L2**：`POST /a`，请求体 16 MiB，响应体同样是 256 KiB。服务端在读完大于 1 MiB 的请求体之前额外等 150 ms；这把“写请求体”也变成会返回 `Pending` 的地方（只在调用方 future 真的负责写请求体时，例如 awc；hyper/reqwest 的写发生在内部连接任务里，isahc 在 curl agent 线程里，调用方 future 看不到——§3.5 与 §9）。

L1/L2 不是第二个目标操作：两者都是“一个普通请求”的输入；四个层级、同一个 Dropwise 配置、同一种 Pending 手段在每个形状内部完全一致。

### 1.3 让请求停在等待回复处的手段（四个层级相同）

用同一个进程内 HTTP/1.1 服务器，对每次请求：

1. 收完请求；请求带 `Expect: 100-continue` 时先回 `100 Continue`（否则 isahc/curl 会等自己的 1 s 兜底）；L2 在请求体 > 1 MiB 时再等 150 ms 才开始读请求体；
2. 等 40 ms 后写响应头；
3. 再等 80 ms 后写响应体；
4. 响应体是 `repl:<X-DW-Id>` 加 256 KiB 的填充。

- 2、3 是“慢的进程内假服务器”，对应 `PLAN.md` §3 允许的手段；4 的填充保证 isahc 这类会把小响应体一次读进内部缓冲的客户端也会在“读响应体”处产生 `Pending` 边界；L2 的 1 是“大负载”。
- 同一目标的四个层级（和 M1-residue）用完全相同的服务器参数；唯一区别是取消之后测什么、怎么判。
- 取消后统一等 **200 ms** 真实时间再复用连接（`RECYCLE_WAIT`）。这比“响应头 40 ms + 响应体 80 ms”长，保证被放弃的回复已经到达客户端（或客户端已经决定关闭连接）。这个等待在 B2/B3/M/M1-residue 中相同；B1 取消后直接结束，不复用。

Dropwise 配置（所有层级、所有目标相同）：`Flavor::MultiThread { workers: 1 }`、`races = [Immediate, AfterWake]`、`max_cancellations = 1`、`max_runs = 128`、`scenario_timeout = 30 s`、`Settle { tokio_time: 50 ms, .. }`。每项重复 3 次。

### 1.4 服务端与观察者

- `http_server.rs` 在 `127.0.0.1:0` 上起一个独立的 HTTP/1.1 服务器，每个 scenario run 一个新实例。它按连接顺序处理请求，**在回复之前**把 `(method, path, X-DW-Id, body 前缀, 连接号)` 记进自己的 `Vec<Record>`。
- 响应体是 `repl:<id>`：客户端拿到的回复必须与它自己请求的 `X-DW-Id` 对应。每个请求的 id 在进程内唯一，因此“读到上一条回复”会表现为 body 不匹配，而不是巧合的相同。
- M1 的幸存者 B 是一个带唯一 id 的 `POST /effect`（body 里也有同一个 marker）；第二个幸存者 C 是 `GET /c`。判定值同时检查：
  1. 服务端日志里 **恰好一次** 出现 B 的 id，且该记录的 method/path 是 `POST /effect`（服务端按这个请求处理了，而且没有重复处理）；C 同理：恰好一次 `GET /c`；
  2. 客户端 B/C 的返回体分别等于 `repl:<B id>` / `repl:<C id>`（拿到的是**这个**请求的回复）；
  3. B/C 都必须返回 `Ok`，超时算失败。
- 判定值（`Obs`）不包含被取消请求 A 的任何结果，也不包含残留字节（M 版）；M-residue 版把复用前客户端 socket 中未读字节的最大值加进观察值，单独统计。
- 独立观察者是服务端自己的记录，不经过被测客户端；每个 run 用自己的服务器实例，因此不会把上一轮/上一个计划的请求算进来。

### 1.5 四个层级 + M1-residue

| 层级 | 取消之后做什么 | 判定 |
|---|---|---|
| B1 | 场景结束 | `dropwise::explore`：不 panic、不超时、无 Obligation 泄漏 |
| B2 | 等 200 ms，用**相同参数**（同 path、同 id、同 body）重做同一请求 | 只看返回 `Ok`；超时算失败 |
| B3 | 等 200 ms，做**不同**的同类请求（L1 换 `/b`；L2 换 `/b` 且请求体为 4 MiB） | 只看返回 `Ok`；超时算失败 |
| M | 等 200 ms；B 发 `POST /effect`（唯一 marker），C 发 `GET /c` | `explore_observed(Expect::Atomic)`；观察值含 B/C 的返回与两端是否一致、服务端是否收到；**不含残留字节** |
| M1-residue | 同 M，但客户端经过 `dropwise::net::Proxy`；在 B 开始前读 `Proxy::residue()` | 同 M，另加未读字节数；单独统计，不计入 Q1 |

注意 M 的一个性质（不是违反）：A 不在观察值里，所以 pre 校准（A 在第一次 poll 前被丢弃）和 post 校准（A 完成）的观察值相同；Dropwise 会把它们判成 `Post`（分类时先看 post），并给出一条“pre 与 post 相等”的退化提示。这是 M1 的预期形状：它只能看幸存者是否被污染，不能证明 A 是否发生。

## 2. 总表（release 版，3 轮）

`计划数` 和 `违反数` 都按第 1/2/3 轮列出。所有轮的 `baseline_errors=0`、`unsettled=0`、`exhaustive=true`；`未实现` 是 Pending 边界在实时调度下没有到达的计划数，按 `PLAN.md` §4 不算通过也不算违反。

| 目标 | 形状 | 层级 | 计划数（第1/2/3轮） | 违反数（第1/2/3轮） | 未实现（第1/2/3轮） | M 的 post 判定（第1/2/3轮） |
|---|---|---|---:|---:|---:|---:|
| hyper-util | L1 | B1 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| hyper-util | L1 | B2 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| hyper-util | L1 | B3 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| hyper-util | L1 | M | 8/8/8 | 0/0/0 | 0/0/0 | 8/8/8 |
| hyper-util | L1 | M-residue | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| hyper-util | L2 | B1 | 8/8/8 | 0/0/0 | 1/0/0 | — |
| hyper-util | L2 | B2 | 8/8/6 | 0/0/0 | 0/0/0 | — |
| hyper-util | L2 | B3 | 8/8/8 | 0/0/0 | 0/0/1 | — |
| hyper-util | L2 | M | 8/8/8 | 0/0/0 | 1/0/0 | 8/8/8 |
| hyper-util | L2 | M-residue | 4/6/6 | 0/0/0 | 0/0/0 | 4/6/6 |
| reqwest | L1 | B1 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| reqwest | L1 | B2 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| reqwest | L1 | B3 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| reqwest | L1 | M | 8/8/8 | 0/0/0 | 0/0/0 | 8/8/8 |
| reqwest | L1 | M-residue | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| reqwest | L2 | B1 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| reqwest | L2 | B2 | 8/8/8 | 0/0/0 | 0/0/0 | — |
| reqwest | L2 | B3 | 8/8/8 | 0/0/0 | 0/1/0 | — |
| reqwest | L2 | M | 8/8/8 | 0/0/0 | 0/0/1 | 8/8/8 |
| reqwest | L2 | M-residue | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| pingora | L1 | B1 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| pingora | L1 | B2 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| pingora | L1 | B3 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| pingora | L1 | M | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| pingora | L1 | M-residue | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| pingora | L2 | B1 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| pingora | L2 | B2 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| pingora | L2 | B3 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| pingora | L2 | M | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| pingora | L2 | M-residue | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| isahc | L1 | B1 | 10/8/8 | 0/0/0 | 1/0/1 | — |
| isahc | L1 | B2 | 8/10/10 | 0/0/0 | 0/1/0 | — |
| isahc | L1 | B3 | 10/6/8 | 0/0/0 | 1/0/0 | — |
| isahc | L1 | M | 10/14/8 | 0/0/0 | 0/5/0 | 10/14/8 |
| isahc | L1 | M-residue | 10/8/16 | 0/0/0 | 1/0/5 | 10/8/16 |
| isahc | L2 | B1 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| isahc | L2 | B2 | 10/10/6 | 0/0/0 | 0/0/0 | — |
| isahc | L2 | B3 | 10/14/10 | 0/0/0 | 0/4/0 | — |
| isahc | L2 | M | 10/10/10 | 0/0/0 | 1/1/0 | 10/10/10 |
| isahc | L2 | M-residue | 14/10/12 | 0/0/0 | 4/0/2 | 14/10/12 |
| awc | L1 | B1 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| awc | L1 | B2 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| awc | L1 | B3 | 6/6/6 | 0/0/0 | 0/0/0 | — |
| awc | L1 | M | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| awc | L1 | M-residue | 6/6/6 | 0/0/0 | 0/0/0 | 6/6/6 |
| awc | L2 | B1 | 28/34/24 | 0/0/0 | 1/5/1 | — |
| awc | L2 | B2 | 32/34/32 | 0/0/0 | 3/8/2 | — |
| awc | L2 | B3 | 34/26/32 | 0/0/0 | 5/1/3 | — |
| awc | L2 | M | 34/28/34 | 0/0/0 | 2/0/9 | 34/28/34 |
| awc | L2 | M-residue | 22/30/30 | 0/0/0 | 0/4/2 | 22/30/30 |

- **版本**：表中只有 release 版；fixed 版 5 个目标 × 10 个层级 × 3 轮 **未运行**（没有补丁，见 §8）。
- **M / M-residue 的 `post`**：由于 pre 与 post 观察值相等（§1.5），表中 M 行在“判定”列给出 `post` 计数；pre 都是 0，`neither`、`unknown` 都是 0。M 的计划数少于 M-residue 的地方是同一轮实时调度造成的计划集差异，不是两个变体用了不同数量的边界。
- **代码与表中数字**：主表来自 2026-10-18 18:16–18:40Z 的 3 轮 release 运行。之后只把 `Obs.detail`（不参与 `PartialEq`，只用于诊断输出）截短以避免日志过大，没有改判定逻辑，主表没有重跑；阳性对照和 trace 用的是截短后的最终代码。
- **未实现是覆盖缺口，不是违反。** isahc 和 awc 的 L2 未实现数较多（最多 9），主要来自 16 MiB 请求体在实时调度下写入/读取的边界数变化；这些计划没有测到计划的取消点，不能当作该取消点的结果。

## 3. 逐目标

### 3.1 hyper-util 旧版 `Client`（目标 1）

- **操作**：完整 `GET`（L1）/ `POST` 16 MiB（L2）；池是 hyper-util legacy `Client` 的私有连接池。
- **Pending 手段**：服务端 40+80 ms 两段延迟 + 256 KiB 响应体填充；L2 加 16 MiB 请求体与 150 ms 读体延迟。
- **结果**：L1 每轮 8 个计划；L2 每轮 4–8 个计划（M-residue 一轮 4）；10 个项目层级全部 0 违反。未实现 0–1。
- **分诊**：没有违反，无需分诊；没有确认的问题、契约外行为或测试框架错误。
- **trace（DW_TRACE=1，单独一轮，不计入总表）**：取消后 A 有时已被服务端完整收到；B 有时复用同一条连接、有时用新连接，两种情况下 B/C 都拿到各自正确的回复，`residue=0`。这与“请求 future 被丢弃后，hyper 的连接任务读完/丢弃响应，再把干净的连接还池，或者关闭连接”一致；我们没有为这个行为打补丁，也没有把它归因成缺陷。

### 3.2 reqwest（目标 2）

- **操作**：完整请求；reqwest 的解码后的 body 通过 `Response::text()` 读完。
- **Pool**：`http1_only()`、每个 host 最多 1 条 idle 连接；`default-features = false`（无 TLS、无 HTTP/2）。
- **结果**：L1、L2 每轮 6–8 个计划（B2-L2 一轮 6）；10 个项目层级全部 0 违反，未实现 0–1。
- **分诊**：同 3.1，无违反。
- **trace**：与 hyper 相同的形状（reqwest 构建在 hyper/hyper-util 上）：A 有时被服务端收到，B 有时同连接、有时新连接，B/C 始终正确，残留 0。

### 3.3 pingora-core + pingora-pool（目标 4）

- **操作**：`Connector::get_http_session` → `write_request_header` →（L2）`write_body`/`finish_body` → `read_response` → `read_body_bytes` 到 EOF → `release_http_session`。池大小 1。
- **Pending 手段**：同上的两段延迟；`write_body` 也在调用方 future 里，但本轮 L2 的计划数仍是 6（3 个边界），没有额外观察到“写请求体”的边界；这是覆盖限制，不表示那里一定没有 Pending。
- **结果**：L1、L2 每轮都是 6 个计划；10 个项目层级全部 0 违反，未实现 0。
- **分诊**：无违反。
- **trace**：取消后 B 在同连接或新连接上都正确；`residue=0`。从 trace 看，session 被 drop 后连接没有带着欠回复回到池；本实验没有发现它把这样的连接交给下一个使用者。

### 3.4 isahc（目标 6）

- **操作**：完整请求，`send_async` + `AsyncReadResponseExt::bytes()` 读完响应体。
- **Pending 手段**：两段延迟 + 256 KiB 响应体；L2 的 16 MiB 请求体写在 curl 的 agent 线程里，调用方 future 看不到写侧的 Pending（L2 的计划数因此并不比 L1 多，反而在部分轮更少）。
- **结果**：L1 每轮 6–16 个计划，L2 每轮 6–14 个计划；10 个项目层级全部 0 违反；未实现 L1 0–5、L2 0–4。
- **分诊**：无违反。
- **trace**：A 被收到时，B 有时同连接、有时新连接；B/C 全对，`residue=0`。isahc 的公开文档明确说：响应体在读完前被 drop，连接会被终止（`AsyncReadResponseExt::consume` 的说明）；这与观察一致。

### 3.5 awc（目标 7）

- **操作**：完整请求；`Client::builder().finish()` 的默认池，`send()`/`send_body()` + `response.body().await`。
- **Pending 手段**：两段延迟 + 256 KiB 响应体；awc 的请求体确实在调用方 future 里写，L2 的 16 MiB 请求体因此带来大量写侧 Pending：L2 每轮 22–34 个计划。
- **结果**：L1 每轮 6 个计划；L2 每轮 22–34 个计划；10 个项目层级全部 0 违反；未实现 L2 0–9。
- **分诊**：无违反。L2 的未实现比例较高，说明实时调度下部分 16 MiB 写入边界没有被到达；这是覆盖限制，不是违反。
- **trace**：L2 的 trace 里大量取消发生在请求体写完之前（A 未出现在服务端日志中），B 多在新连接上；少数 A 已完整收到时，B 在同连接上也能正确拿到自己的回复，残留 0。

### 3.6 为什么 0 个“M1 抓到、B3 没抓到”

M 没有报出任何违反；B2/B3 也没有。没有可比较的计划，因此没有低层级解释条目。B3 的已知弱点（只看 `Ok`、不看返回体）在这批目标上没有被触发：所有 B3 的重试/不同请求都返回了 `Ok`，而 M 的幸存者在同样计划下也全部拿到了自己的回复。

## 4. Q3：残留字节

- **可用性**：5/5 个目标都能走 `dropwise::net::Proxy`（Linux `FIONREAD`），M1-residue 都实际跑完。
- **结果**：所有目标、所有轮、所有计划的最大残留字节都是 **0**。表中每个 M-residue 行都有对应的 `LADDER-RESIDUE ... max_residue=0`。
- **阳性对照**：故意把连接在读完回复前还池的 buggy 客户端，M-residue 观察到 262250–262252 字节，并报出违反（§6）。因此 0 不是探针不工作，而是这 5 个客户端在取消后没有把欠回复的客户端 socket 留给幸存者：它们要么在内部把回复读掉后复用连接，要么关闭连接。
- **限制**：`Proxy::residue()` 只看客户端 socket 的未读字节，不看客户端库内部缓冲；因此“内部缓冲里还欠回复但 socket 已空”的情况它看不到。M（不含残留）的用户可见判定仍能抓到“错位回复”；本次 M 也没有报。

## 5. 成本

时间来自本次会话的实际文件时间戳和 `run-http.sh` 的 START/END 行；四类比例是对工作内容的估计，不是计时器输出。共享环境（服务器、harness、runner、阳性对照、文档）只做一次，按目标数均摊；表中的四类比例只指该目标新增的工作。

| 目标 | 工作开始（UTC，约） | 最后一轮结束（UTC） | 该目标新增/适配代码（非注释行） | 运行窗口（3 轮） | 估计比例：搭环境/制造 Pending/写观察者/判定 |
|---|---|---|---|---|---|
| hyper-util | 2026-10-18 17:48Z | 2026-10-18 18:20Z | 44（adapter+imports+11 个 test 外壳） | 3m49s | 40% / 20% / 15% / 25% |
| reqwest | 2026-10-18 17:56Z | 2026-10-18 18:24Z | 37 | 3m58s | 40% / 20% / 15% / 25% |
| pingora | 2026-10-18 17:52Z | 2026-10-18 18:27Z | 50 | 2m49s | 50% / 15% / 15% / 20% |
| isahc | 2026-10-18 17:54Z | 2026-10-18 18:32Z | 37 | 4m57s | 50% / 20% / 10% / 20% |
| awc | 2026-10-18 17:55Z | 2026-10-18 18:40Z | 46 | 8m38s | 55% / 20% / 10% / 15% |

场景代码行数（定义同 RESCAN 6.8：非空、首字符不是 `/`；不含共享服务器）：

| 部分 | 行数 | 说明 |
|---|---:|---|
| B1 场景体（L1/L2 共用） | 6 | `scenario_b1`；所有目标共用 |
| B2 场景体（L1/L2 共用） | 18 | 重试 + 200 ms 等待 |
| B3 场景体（L1/L2 共用） | 19 | 不同请求 |
| M 场景体（L1/L2 共用） | 30 | 不含 14 行 `DW_TRACE` 诊断和 22 行 residue 分支 |
| M1-residue 额外 | 22 | residue 采样 + 并入观察值（与 M 共用其余代码） |
| 每个层级的 test 外壳 | 4 | `#[test]` + 函数 + 调用 + `}`；每个目标 11 个（L1/L2 的 B1–B3/M/residue + reuse） |
| 目标适配器 | 32–41 | 客户端构造 + 一次完整请求；因 crate 而异 |
| 共享环境 | 626 | `http_server.rs` 175 + `harness.rs` 389 + `run-http.sh` 62（非注释行） |
| 阳性对照 | 91 | `control_ladder.rs`，不计入目标 |

共享环境约占一次性 30–35 分钟；每个目标自身的适配、调试、运行、判定约 15–30 分钟，均在“半天”预算内。

## 6. 阳性对照与连接复用

**阳性对照**（`tests/control_ladder.rs`，不属于任何冻结目标）：一个故意在读完回复前把连接放回槽位的客户端。运行结果：

```
LADDER M CONTROL [control]: plans=2 violations=2 unrealized=0 ... pre=0 post=0 neither=2 unknown=0
LADDER-RESIDUE M-residue CONTROL [control]: max_residue=262252
LADDER M-residue CONTROL [control]: plans=2 violations=2 ... pre=0 post=0 neither=2 unknown=0
```

两个计划的 B 都读到 `repl:A-...`（A 的回复）而不是 `repl:B-...`，C 读到 B 的回复；服务端同时记录了 B、C。这证明服务器日志、回复 id、`Obs` 与 residue 探针都能报告真实的错位，0 结果不是“观察器永远返回 pre/post”。

**连接复用诊断**（`reuse`，不计入总表）：每个客户端连续发 4 个不取消的请求，服务端日志显示 `requests=4 reused=true`，4 个请求都在连接 0 上。因此池确实会复用，零结果不是“池没有复用”的假象。

**trace 汇总**（`DW_TRACE=1`，单独各跑一轮 M-residue，L1+L2；只作诊断，数字不进入总表）。以下数字来自单独的一轮 `DW_TRACE=1` M-residue 运行（包含 pre/post 校准 run，所以不是上表的计划数）；`A 被服务端收到` 表示服务端记录到了完整的 A 请求，取消发生在请求体写完之前时它是 0。它只用于说明幸存者在什么连接上看到了什么，不进入 Q1。

| 目标 | 形状 | traced runs | A 被服务端收到 | B 复用同连接 | B 用新连接 | B/C 全对且恰好一次 | 最大残留 |
|---|---|---:|---:|---:|---:|---:|---:|
| hyper-util | L1 | 11 | 8 | 8 | 3 | 11 | 0 |
| hyper-util | L2 | 11 | 8 | 8 | 3 | 11 | 0 |
| reqwest | L1 | 11 | 8 | 8 | 3 | 11 | 0 |
| reqwest | L2 | 11 | 8 | 8 | 3 | 11 | 0 |
| pingora | L1 | 11 | 7 | 5 | 6 | 11 | 0 |
| pingora | L2 | 11 | 7 | 5 | 6 | 11 | 0 |
| isahc | L1 | 17 | 15 | 11 | 6 | 17 | 0 |
| isahc | L2 | 9 | 6 | 5 | 4 | 9 | 0 |
| awc | L1 | 11 | 9 | 5 | 6 | 11 | 0 |
| awc | L2 | 23 | 3 | 5 | 18 | 23 | 0 |

## 7. 分诊

| 类别 | 数量 | 说明 |
|---|---:|---|
| 确认的问题 | 0 | 没有任何计划在至少一轮里稳定地表现出用户可见的后果；因此没有补丁/配置对照，也没有下游使用者会看到什么可写 |
| 契约外行为 | 0 | 没有需要归入“文档不保证/协议本身不原子”的违反 |
| 测试框架错误 | 0 | 所有 baseline 都能完成，所有计划都有 `baseline_errors=0`、`unsettled=0`；未实现计划单独计数，没有算成违反 |

## 8. 偏离、修订与事故

- 冻结文件 `PLAN.md`、`targets.md`、`FREEZE.txt` 未改；`sha256sum PLAN.md targets.md` 与 `FREEZE.txt` 一致。
- `AMENDMENTS.md` 第 8–13 条记录本次的 6 项偏离：结果文件名、L1/L2 两种请求形状、Pending 手段与响应填充、M1-residue 只用 Proxy 做残留探针、`reuse` 诊断、以及没有跑 fixed 版。
- **fixed/补丁版未运行**：release 版没有任何确认的问题，因此没有可写的补丁或配置对照；`run-http.sh` 的 `[patch.crates-io]` 路径已用一份只加注释的 no-op patch 对 hyper 1.12.0 做过一轮冒烟验证（构建、复制、支持模块的绝对路径改写都工作），随后删除了该 patch，`patches/MANIFEST` 恢复为空；这次冒烟数字不进入任何结果。空 MANIFEST 的跳过分支也已实际执行。
- **并发事故（不是本实验的设计偏离）**：另一个会话在 2026-10-19 02:51 JST 把 `src/lib.rs` 覆盖成一行占位注释，导致整个仓库无法编译。我在 02:52 JST 用 `git show HEAD:src/lib.rs` 恢复了原文件；之后该会话的 rustfmt 只重排了 `pub use`（git diff 只有格式差异），库的 API/语义没有被我改动。本实验没有改 `src/` 下的其他内容。
- 没有 commit、push、issue、PR 或上游联系；没有启动 Docker 容器。

## 9. 没做/局限

- 没有 fixed 版数字；没有补丁，因为零发现。
- L2 的写侧取消只在调用方 future 负责写请求体时可见（awc；pingora 本轮没有显示额外边界）；hyper/reqwest/isahc 的写发生在内部任务/线程里，本实验的方法看不到。
- 每个目标只测了一个“普通请求”操作；没有会话状态调用（这些客户端没有）。没有 TLS、HTTP/2、真实服务器、重定向、chunked 响应、并发多客户端或连接池耗尽。
- 每种形状只有一个响应体大小（256 KiB）和一个请求体大小（16 MiB）；没有扫参数。
- 实时调度下计划数在轮间浮动，并有未实现计划；逐计划比较只在同一轮内成立。M 的计划数与 M-residue 的计划数也不一定相同。
- M 的 pre/post 观察值相等（§1.5），是设计使然；它只能证明幸存者没有被污染，不能证明 A 是否发生。
- 5 个目标是冻结清单按下载量选的，不是随机样本；没有盲法。
- 阳性对照证明观察器能抓错位，但它不能证明这 5 个客户端在所有输入/调度下都干净。

## 10. PRIVATE-NOTES

无。没有发现可能涉及安全的问题（没有出现“下一个使用者读到别的请求的回复内容”这类用户可见后果），因此没有写 `PRIVATE-NOTES.md`。

## 11. 复现

```sh
cd /home/ava/Desktop/Dropwise
REPS=3 bash prospective/phase3/run-http.sh
# 或只跑 release，指定目标：
TARGETS="isahc awc" VARIANTS=release REPS=3 bash prospective/phase3/run-http.sh
```

需要 Linux（`dropwise::net` 的 `FIONREAD`/Proxy）、能联网取 crates（首次 `cargo fetch`）、无 Docker/服务端依赖。脚本用 `--features net` 构建，每个目标一个测试二进制，`--test-threads=1 --nocapture`。
