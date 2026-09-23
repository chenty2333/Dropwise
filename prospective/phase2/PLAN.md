# Phase 2: application-level cancellation study plan

**Plan fixed:** 2026-09-23 UTC, before writing scenarios or injecting cancellation. This is a new study round, not an amendment to or a rewrite of the frozen T1–T3 registration. No result from this round will be merged into `prospective/RESULTS.md`.

## 1. Workspace and scope

The repository was on `master` at `3ec33ec` with a clean worktree. The concurrent Codex project-evaluation task reported that it had made no repository edits and had no write planned; the only observed Claude process was working in `/home/ava/Desktop/paf`. No conflicting Dropwise edits were present.

At most six candidates were screened using repository membership, existing target/reproduction scope, and normal API documentation/source only; no cancellation issue, repair PR, or defect history was searched. Selected targets are both outside the 27 survey repositories and the existing reproductions:

| Candidate | Decision | Reason |
|---|---|---|
| `moka` 0.12.16, `moka::future::Cache::{get_with,get}` | **Select** | Independent concurrent cache; documented per-key initializer coalescing gives a precise shared-task/waiter condition. |
| `async-nats` 0.50.0, JetStream pull consumer / message ack | **Select** | Independent client; exercises a live protocol stream and the responsibility of an acquired, explicitly acknowledged message against a local server. |
| `tower` 0.5.3, `tower::buffer::Buffer` | Screened, not selected | Its docs/source describe buffered request/response flow and deliberately discard queued requests whose caller has gone away. Whether an operation should commit is predominantly the downstream service's policy; this would make attribution weaker than the two selected cases. |
| `lapin` 4.12.0, AMQP consumer | Screened, not selected | Same broker delivery/ack/requeue mechanism as the selected JetStream scenarios; an additional broker would expand scope without adding a distinct mechanism. |
| `deadpool` 0.13.1 | Screened, not selected | Pool checkout overlaps the already completed bb8/sqlx work and adds no distinct application mechanism for this round. |
| `flume` 0.12.0 | Screened, not selected | A channel primitive rather than a caller-level workflow; its behavior would be dominated by our small harness rather than a target application's integration. |

No candidate is an established application repository's existing integration test. The test programs will be authored here and will call the selected projects' real public APIs; conclusions will distinguish this integration harness from upstream application code.

## 2. Correctness conditions and scenes

All findings are exploratory unless directly entailed by the cited public behavior. Absence of a cancellation guarantee alone is not a violation. In every mode, each run creates fresh state with the same keys/messages, gates, caller recovery branch, and end condition.

### Moka — shared initialization / waiter continuation

Public basis: `Cache::get_with` says concurrent calls for one absent key are coalesced into one initializer evaluation and the other calls wait for its result ([versioned API docs](https://docs.rs/moka/0.12.16/moka/future/struct.Cache.html#method.get_with)). That text does not explicitly promise cancellation safety, so behavior specifically after cancel is exploratory.

- **M2.1, initializer caller cancelled:** one caller starts `get_with` for an absent key with a gated loader; two same-key callers are already waiting. Cancel only the initiating call, then let the waiting callers and a fresh same-key lookup continue. Allowed: dropping/restarting the loader, promoting another in-flight initializer, caller retry, and duplicate delivery of the same cached value. Required for this harness: surviving callers and the retry finish within the bound and all return the same value as `Cache::get`; no stuck key or conflicting value. This is a proposed application-level recovery condition, not a library cancellation promise.
- **M2.2, one waiter cancelled:** one gated initializer and two waiters call `get_with`; cancel one waiting call while the initiating caller and other waiter remain. Allowed: the cancelled caller receives no value. Required: the initializer continues once, remaining callers and a later cache read return its value. A cancellation that strands/corrupts the remaining callers is a candidate failure of the tested application condition; cancellation safety itself is not documented.

Caller recovery after any cancellation is the same in A/B/C: keep the initiating/waiting work that remains, perform a fresh lookup when M2.1 cancelled its initiating caller, and verify all surviving values and the cache entry. Loader phases will use event gates, not sleeps.

### async-nats — stream continuation and message responsibility

Public basis: `PullConsumer::messages()` returns a message stream; a pull consumer uses explicit acknowledgements, unacknowledged messages may be redelivered, and `double_ack` waits for server confirmation ([consumer docs](https://docs.rs/async-nats/0.50.0/async_nats/jetstream/consumer/struct.Consumer.html), [message docs](https://docs.rs/async-nats/0.50.0/async_nats/jetstream/message/struct.Message.html), [consumer config](https://docs.rs/async-nats/0.50.0/async_nats/jetstream/consumer/pull/struct.Config.html)). The docs do not promise that a pending `StreamExt::next` is cancel-safe. Stream continuation after cancellation is therefore exploratory; explicit-ack/requeue and at-least-once behavior are the documented basis for the recovery scenario.

- **N2.1, pending read then continue same stream:** receive and acknowledge a prefix message, then race `messages.next()` while no later item has been published. An event emitted when the future actually returns `Pending` releases a producer to publish two unique IDs. After cancellation, continue on the **same** stream and acknowledge/process the remaining logical IDs. Required exploratory condition: neither ID is lost or response-mismatched and the stream advances; duplicates are tolerated by an idempotent sink because JetStream is at-least-once.
- **N2.2, acquired delivery then handler cancellation:** acquire one unacknowledged delivery, run an event-gated handler whose effect is recorded in an idempotent in-process sink, and race the handler inside the caller's `select!`. On cancellation, explicitly `Nak` the held delivery, receive the redelivery on the same durable pull consumer, finish the idempotent effect, and `double_ack`; then consume and acknowledge a second unique message to demonstrate forward progress. Required: both logical effects exist exactly once, the first message is either committed-and-acknowledged by the common recovery path or redelivered and acknowledged, and no message remains pending at the end. At-least-once redelivery and deduplication are allowed. The in-process sink is caller test logic, not a claim that async-nats provides transactionality with an external database.

The service will be a temporary official `nats:2.11.6-alpine` image with JetStream enabled. Its dynamically published client port will bind only to `127.0.0.1`; the runner will stop and remove the container on every exit.

## 3. Comparison and fixed settings

- **A — baseline:** no cancellation injection; execute the normal operation and the same post-operation/recovery checks.
- **B — simple boundary baseline:** an observer records a target future's `Poll::Pending`; a one-shot competing branch becomes ready on the first selected Pending and is placed first in `tokio::select!`, immediately dropping that future. This is a deliberately small one-cut harness, **not** a claim to reproduce all capabilities of `futures-testing` or any other existing tool.
- **C-matched — budget matched:** Dropwise `ctx.race` with `races=[Immediate]`, `max_cancellations=1`, and `max_runs=2` (baseline plus one first-boundary cancellation plan). This is the direct one-plan comparison with B.
- **C-full — preselected full schedule set:** Dropwise `ctx.race` with `races=[Immediate, Reschedule(1), After(10ms)]`, `max_cancellations=1`, `max_runs=128`, `Settle::default()`, `scenario_timeout=10s`, and `Flavor::MultiThread { workers: 2 }`. All configuration values are fixed before scene code and injection. Multi-thread/real time is used because the NATS scenario requires TCP progress; `After(10ms)` is real time here and provides a preselected extra scheduling window, while event gates establish the required input order. Outcomes, unrealized plans, incomplete search, and observation cutoffs will be reported explicitly.

A/B/C use the same invocation, input IDs, gates, and caller recovery/check function; only the mechanism and point of cancellation differ. Each scenario/mode is repeated three times, sequentially. Since C-full spends more cancellation plans, it will not be credited with a raw detection advantage over B; C-matched supplies the equal-one-plan comparison. Report per-run and aggregate elapsed time, plan/actual-cancel counts, and the full suite wall time separately. Timing covers scenario execution; service startup and compile time are reported separately.

## 4. Execution order and budget

1. Commit this plan before writing scenes or injecting cancellation.
2. Write the complete four scenarios and runners, run compile-only tests (`cargo test --no-run`), and commit the scene code.
3. Run A three times for **all** scenes first. Do not inject cancellation unless every baseline completes and satisfies its conditions. Any scene correction is committed and logged before injection.
4. Run B, C-matched, then C-full, each three times per scene. Save each run's concise metrics and full Dropwise report / harness output under `prospective/phase2/runs/`.
5. Triage outcomes in order: baseline/tool/scene error; condition outside documented/allowed behavior; only then a reproducible candidate in the actual project implementation. Preserve any pre/post-correction output and log changes; a candidate requires minimization and source explanation before calling it a project defect.

Scope cap: two targets, two scenarios each, three repetitions per method, four methods (A/B/C-matched/C-full), and at most 128 runs within each C-full `explore()` call; each marked scenario is bounded at 10 seconds plus default settling. No target expansion, generic runner framework, Dropwise implementation work, upstream issue/PR, or external service is planned. If full search reaches the cap, it will be reported as incomplete rather than increased after seeing results. Scene authoring time will be measured from first scenario-code edit through the compile-success scene commit; selection/reading time is not counted as code-writing time.

## 5. Related testing approaches consulted

The comparison is not presented as a first proposal of per-Pending cancellation. Hyperactor's documented helper counts yield points and, for each boundary, drops a fresh run and verifies a fresh run's output; its async form accepts an `on_pending` driver for external progress ([API docs](https://meta-pytorch.org/monarch/stable/rust-api/hyperactor/testing/cancel_safe/index.html), [implementation](https://meta-pytorch.org/monarch/stable/rust-api/src/hyperactor/testing/cancel_safe.rs.html)). `futures-testing` is a property-based leaf-future harness that randomizes polls, driver progress, cancellation, spurious polls, and waker swaps with reproducible seeds ([README and usage](https://github.com/conradludgate/futures-testing)). B here tests one observed boundary and is intentionally much narrower; Dropwise C adds caller `select!` recovery, post-cancellation checks, and selected race timing. No benchmark or capability claim will be made about either reference tool.
