# Dropwise exploration API — investigation report

**Scope.** Read-only inspection of `/home/ava/Desktop/Dropwise` at commit `6335c6c5…`; `Dropwise-online` is a worktree of the same commit (`src/explore.rs` and `tests/observed.rs` hashes identical), so all refs apply to both. Crate 0.1.0, edition 2024, Rust 1.88 (`Cargo.toml:1-12`). Unprefixed `:line` refs are to `src/explore.rs`; other files are named explicitly. Only this report was written.

`explore` searches plans for invariant/leak violations; `explore_observed` also calibrates the scenario's *returned value* under "never started" (`pre`) and "not cancelled" (`post`) and assigns a `Verdict`. Plans cancel marked futures after a chosen number of observed `Pending` returns, not source-level `.await`s.

---

## 1. `explore` vs `explore_observed`; plans, boundaries, race, repeat, gap

### 1.1 Signatures and difference

```rust
pub fn explore<S, Fut>(config: &Config, mut scenario: S) -> Report
where S: FnMut(Ctx) -> Fut, Fut: Future<Output = Result<(), String>>;        // src/explore.rs:1713-1717
pub fn explore_observed<T, S, Fut>(config: &Config, expect: Expect, mut scenario: S) -> Report
where T: PartialEq + fmt::Debug + Send + 'static,
      S: FnMut(Ctx) -> Fut, Fut: Future<Output = Result<T, String>>;        // src/explore.rs:2121-2126
```

Both use the same BFS `search` (`:1736-1774`): the first run is `Plan::default()` (baseline, no cuts), recorded as baseline rather than a `Trial` (`:1752-1754`). `explore` supplies `Judgement::default()`, so trials have `verdict: None`/`comparison: None` (`:1718-1723`, `:1754-1765`). `explore_observed` uses `run_observed` and judges only when the scenario returned `Some(Ok(actual))` (`:2128-2141`): "`pre`: every cut target is dropped before its first poll …; `post`: nothing is cancelled and every target completes; for a plan with several cuts under `Expect::Atomic`, every mixture …" (`:2087-2097`).

### 1.2 What a plan is; how boundaries are chosen

> "One cancellation plan: which targets lose, after which observed `Pending`, with which competitor timing, and which gap substitution." (`Plan` doc, `:231-239`)

```rust
pub struct Cut { pub target: usize, pub after_pending: usize, pub race: Race }       // :223-229
pub struct Plan { pub cuts: Vec<Cut>, pub repeat: bool, pub gaps: Vec<Option<Gap>> } // :240-254
```

A boundary is a `Pending` of the marked future, not an `.await`: "one `.await` may return `Pending` many times, and awaits that complete immediately are never boundaries" (`:216-220`). `after_pending == 0` is calibration-only and never a normal plan (`:221-222`); replay rejects it (`:2291-2293`). Each target appears at most once, cuts ascending by target index (`:241-245`, `:2282-2311`).

Every `Ctx::target`/`Ctx::race` call registers a target index in call order (`TargetCore::register`, `:473-501`); every `Pending` increments a counter, and `after_pending == seen` is the losing boundary (`:526-536`). From each completed run, `extend_queue` adds `after_pending in 1..=rec.pendings` × every `races` mode for every reached target (`:1654-1661`), only for *later* target indices so combinations occur once (`:1647-1648`). Search is baseline, singles, pairs, … up to `max_cancellations` (`:1640-1646`, `:1707-1712`); the queue is budgeted by `max_runs` and overflow sets `truncated` (`:1695-1701`, `:1767-1772`). Counts come from the run just executed, not necessarily the baseline. If no marked target ever returns `Pending`, no plan is generated; when the search is not truncated, `coverage_gaps()` reports this (`:993-994`, `:1050-1056`; `tests/product.rs:380-403`).

### 1.3 `race`, `Race`, `Preempt`

> "When the competing branch wins, measured from the moment the target returns the planned `Pending`." (`:13-14`)

`Race::Immediate` (ready at once), `Reschedule(usize)` (after N `yield_now` calls; "**not guaranteed**: the runtime may re-poll immediately", `:20-23`), `After(Duration)` (Tokio time: virtual under `CurrentThread`, real under `MultiThread`, `:24-25`), `AfterWake` (ready once the target is woken; target dropped unpolled; requires the program's `select!` to possibly poll the competitor first — do not use with a `biased;` select listing the target first, `:27-35`). Full enum: `:16-36`.

```rust
pub fn race<F: Future>(&self, fut: F) -> (Raced<F>, Preempt)   // :354-355
```

"When the plan says the target loses, it stops making progress and `Preempt` becomes ready; the program's `select!` then drops (or keeps) the target exactly as it would for a real competitor." (`:340-345`). `Preempt` is "Ready exactly once … pending forever otherwise, so it can be reused across loop iterations with `&mut preempt`" (`:818-820`); after firing, later polls return `Pending` (`:844-869`). Dropping an unfired doomed `Preempt` sets `fired` and wakes the target to avoid a hang (`:883-896`). `AfterWake` uses a `WakeProbe` waker (`:606-654`, `:570-573`).

### 1.4 `repeat_at_site`, `gap_choices`, `Ctx::gap`

`repeat_at_site` adds, "for every single cut, a plan that cancels the target and every later target created at the **same call site** at the same `Pending` boundary" (`:168-171`). The plan holds only the first cut with `repeat: true` (`:248`); later targets at the first cut target's site inherit `after_pending`/`race` and count that boundary at their own pending count (`:482-501`). One plan is generated per eligible single cut when a later same-site target exists (`:1620-1628`, `:1665-1671`). `Trial::cancelled` counts actual instances and may exceed `plan.len()` (`:924-926`, `:1600-1604`); `outcomes` is parallel to explicit cuts only, so a repeated instance that missed its boundary is not `Unrealized` (`:920-926`, `:952-954`). Repeated-site plans calibrate only the extremes (`:2064-2066`, `:2076`).

```rust
pub fn gap(&self, default: Gap) -> GapWait   // :399
pub enum Gap { Now, Yield(usize), Sleep(Duration) }   // :49-64
```

"`default` is what the scenario would do in production. The exploration may substitute a choice from `Config::gap_choices`; calibration and plans without a substitution use `default`." Sites are numbered in call order; "a calibration that skips a target can change which later calls are reachable; keep gap calls on unconditional paths." (`:388-398`). `Gap::Sleep` is Tokio time, virtual under `CurrentThread` (`:62-63`); `GapWait` is poll-safe across `yield_now`/`sleep` (`:414-463`). `gap_choices` expands only **single-cut** plans, one variant per gap site × non-default substitute, skipping duplicates (`:1665-1693`). Assignments are a prefix in `Plan.gaps` (`:249-253`), recorded on `Trial.gaps` (`:927-930`), and part of the calibration cache key (`:1851-1856`). An assignment the run does not register is silently unused during search; only replay rejects it (`:2313-2324`, `tests/product.rs:362-378`).

---

## 2. `Ctx::target` / `Ctx::race` outputs; cancellation; multiple targets; `Trial`

### 2.1 `Ctx::target`

```rust
#[track_caller] pub fn target<F: Future>(&self, fut: F) -> Target<F>          // :317-318
impl<F: Future> Future for Target<F> { type Output = Option<F::Output>; }    // :676-677
```

"Resolves to `None` if it was cancelled in this run, `Some(output)` otherwise. The code after `.await` stands in for the caller's post-cancellation control flow; it is the scenario author's job to make it faithful." (`:311-316`). At the planned boundary the inner future is moved to `Phase::Doomed` and kept unpolled while the competitor delay runs; when it fires, the phase becomes `Done` and the inner future is **dropped before** `Poll::Ready(None)` (`:690-717`, comment: destructors run "where a real `select!` would run them", `:712-713`). Dropping the `Target` from outer code while doomed marks it cancelled and drops the inner future with it (`:728-735`). The `pre` cut drops `fut` inside `Ctx::target` before any poll (`:320-331`, `:718-721`); awaiting that skipped target still returns `None`, so the scenario executes its normal post-cancellation path. A `Target` polled after completion panics (`:722`); it is single-use.

### 2.2 `Ctx::race`

`Raced<F>` outputs `F::Output` (`:759-761`); `Preempt` outputs `()` (`:832-834`). `Raced` does **not** return `Option`; cancellation is observed by which `select!` branch wins. Between losing and the competitor firing it returns `Pending` without polling the inner future (`:772-777`). If the program keeps it and polls again after the competitor fired, it polls the inner future again; if it is still `Pending`, it increments `seen` and marks `Status::Kept` (`:789-806`); if dropped while `Doomed` or `Kept` it marks `Cancelled` (`:810-815`). So `ctx.race` is the only way to retain and possibly continue the losing future; `ctx.target` drops it.

### 2.3 Multiple targets, mixed outcomes, `Trial`

Every call is a new target index; a plan cuts at most `max_cancellations` targets, one boundary each (`:165-166`, `:241-245`). Each future is independent, so completed and cancelled targets can coexist in one run. Per plan cut, `Trial.outcomes` records `CutOutcome::{Cancelled, Kept, Stalled, Unrealized}` (`:898-910`, mapping `:1578-1587`): `Kept` only via `ctx.race`; `Stalled` = boundary reached but competitor never fired; `Unrealized` = target not created or boundary not reached. `Trial.cancelled` counts every record that lost and ended `Cancelled`, including repeated instances (`:1600-1604`). Under `Expect::Atomic`, a ≥2-cut plan gets mixed `pre`/`post` keys so "one completed, one never started" is `Verdict::Mixed`, not a violation (`:2064-2081`, `tests/observed.rs:270-310`).

`Trial` ("Result of one run with a non-empty cancellation plan", `:912-913`) records (`:914-945`): `plan: Vec<Cut>`; `repeat`; `sites: Vec<Option<&'static Location>>` parallel to `plan`; `outcomes: Vec<CutOutcome>`; `cancelled: usize`; `gaps: Vec<Option<Gap>>` (empty = all scenario defaults); `verdict: Option<Verdict>` (observed runs only); `comparison: Option<String>` (diagnosis only, never the oracle); `invariant: Result<(), String>`; `leaks: Vec<Leak>`; `settled: bool`. Methods: `is_violation()` = invariant error or any leak (`:948-950`); `is_realized()` = no `Unrealized` (`:952-954`); `replay_plan()` (`:958-964`); `plan_label()` (`:966-974`).

---

## 3. `Expect`, calibration, `Verdict`, coverage

### 3.1 Contract

| `Expect` | Doc meaning | Allowed verdicts |
|---|---|---|
| `Atomic` | "Abandoning is allowed": observation equals `pre` **or** `post` | `Pre`, `Post`, `Mixed`, `Unknown` (`:2028-2035`) |
| `Completed` | "The caller retries or carries on": equals `post` | `Post`, `Unknown` (`:2036`) |
| `Undone` | "The result must be handed to the caller": only `pre` | `Pre`, `Unknown` (`:2037`) |

Definitions `:76-91`; the contract is declared, not inferred (`:64-69`).

### 3.2 Calibration procedure

* `pre` = all judged targets skipped (`:2073`), via `Cut { target, after_pending: 0, race: Immediate }` (`:1882-1899`): `ctx.target` drops the future before first poll (`:320-331`); `ctx.race` never polls the inner future and its competitor is ready at once (`:358-368`, `:766-771`, `:837-843`).
* `post` = same targets, no cuts, all complete (`:2074`).
* `mixed` = every non-empty proper subset skipped, only if `expect == Atomic && !plan.repeat && targets.len() >= 2` (`:2075-2080`).
* Each side is sampled **twice**; disagreement → `Side::Unstable`, zero samples → `Side::Absent` (`:1948-1989`). Cache key = sorted targets + skip flags + trimmed gaps; plans differing only in `after_pending`/`race` share it (`:1851-1856`, `:1991-2007`).
* Failures (any invariant `Err`, including a scenario `Err`/`after_settle` check, a leak, or no observation) go to `Report::calibration_errors`, not panics or baseline errors (`:1962-1985`); an `Absent` side forces `Unknown`. Calibration runs are not trials and do **not** count against `max_runs` (`:2101-2105`).

### 3.3 What is compared; `Verdict`

Only the returned `T`, via `PartialEq` (`T: PartialEq + Debug + Send + 'static`, `:2123`). There is no observer parameter: an independent observer must be part of `T`. `comparison` is `Debug` text "for diagnosis only … never used as the oracle" (`:936-939`, `:2202-2217`).

```rust
if post.matches(actual) { Post } else if pre.matches(actual) { Pre }
else if mixed.iter().any(|s| s.matches(actual)) { Mixed }
else if pre.undecided() || post.undecided() || mixed.iter().any(|s| s.undecided()) { Unknown }
else { Neither }                                                         // :2009-2026
```

Post is checked first, so `pre == post` gives `Post` (`:96-99`; `tests/observed.rs:160-175`). `Unknown` is allowed by every `Expect` (`:2028-2039`), so it is **never** a violation — it is a coverage gap. A disallowed verdict appends `"{verdict:?} under {expect:?}: {comparison}"` to the trial invariant (`:2187-2188`), making `is_violation()` true. If the trial run returns `Err`, `run_observed` moves it to `res.invariant` and no verdict is produced (`:1813-1818`, `:2131`). If stable `pre` and `post` are equal, a note says the observation "cannot tell whether the operation happened, only whether it contaminated shared state" (`:2041-2060`); notes never affect `is_clean` (`:999-1001`).

### 3.4 Coverage gaps; clean within coverage

`coverage_gaps()` (`:1044-1093`) reports: no target marked and not truncated (`:1046-1049`); no marked target ever returned `Pending` and not truncated (`:1050-1056`); baseline settle watchdog (`:1057-1062`); truncated by `max_runs` (`:1063-1067`); otherwise non-exhaustive with targets (`:1068-1070`); unsettled trials (`:1071-1076`); `Unrealized` trials (`:1077-1082`); `Unknown` verdicts (`:1083-1088`); each calibration error as `"calibration failed: …"` (`:1089-1091`). Violations and baseline errors are deliberately **not** gaps (`:1040-1043`).

```rust
pub fn is_clean_within_coverage(&self) -> bool { self.is_clean() && self.coverage_gaps().is_empty() } // :1101-1103
pub fn is_clean(&self) -> bool { self.baseline_errors.is_empty() && self.violations().next().is_none() } // :1115-1117
```

`is_clean` ignores calibration errors, `Unknown`, `Unrealized`, truncation and unsettled runs; `limits.rs:76-102` and `:104-123` assert it true for an unstable calibration and a missing cancellation point. `assert_observed`/`assert_cancel_correct` check `is_clean_within_coverage()` with default `Config` (`:2416-2445`), so they also fail on `Unknown`, unrealized, unsettled, truncated or no-plan runs. `Display` prints only what needs attention (`:1236-1373`); `diagnostics()` dumps every trial, plan, comparison and gap (`:1119-1200`).

---

## 4. `Config`, search size, determinism

| Field | Default | Effect |
|---|---|---|
| `max_runs: usize` | `1000` | Upper bound on runs **including baseline** (`:162-163`); calibration excluded (`:2104-2105`); exhausting sets `truncated` (`:1695-1701`, `:1771`). |
| `max_cancellations: usize` | `1` | Max cuts per plan; 1 means no pairs/triples (`:164-165`, `:1640-1646`). |
| `races: Vec<Race>` | `[Immediate, Reschedule(1)]` | One plan per boundary per mode (`:166-167`, `:1654-1661`). |
| `repeat_at_site: bool` | `false` | Adds one repeated-site plan per eligible single cut (`:168-179`). |
| `gap_choices: Vec<Gap>` | empty | Single-cut variants per gap site × non-default choice (`:180-184`, `:1672-1693`). |
| `flavor: Flavor` | `CurrentThread` | Paused single-thread vs real-time `MultiThread { workers }` (`:114-123`, `:1375-1399`). |
| `settle: Settle` | `100ms`, `0`, `10s` | Post-scenario observation window (`:186`, `:150-158`). |
| `scenario_timeout: Duration` | `30s` | In-process wall-clock bound; timeout → liveness `Err` (`:187-196`, `:1542-1548`). |

Defaults `:199-212`. Let `p_i` be target `i`'s observed `Pending` count and `r = races.len()`. With `max_cancellations = 1` and enough budget: `runs = 1 + r·Σp_i`, `trials = runs - 1`. Defaults (`r = 2`): one target/one pending → 3 runs / 2 trials; one target/two pendings → 5/4; two targets/one pending each → 5/4. `tests/observed.rs:22-42` (one race → 1 trial) and `:243-265` (two targets, pair under `max_cancellations: 2`, one race → 3 trials) confirm. With `max_cancellations = 2`, each single-cut parent adds `r·p_j` pairs for each later target `j`; two one-pending targets with defaults give 1 baseline + 4 singles + 4 pairs = 9 runs / 8 trials. `explore_observed` adds calibration runs (not counted by `max_runs`): two `pre` + two `post` samples, cached across plans sharing targets/gaps (`:1960`, `:2073-2081`, `:2104-2105`). For any two-trial scenario (e.g. one target/one pending under defaults), the total is 1 baseline + 2 trials + 4 calibration = 7 scenario invocations; `tests/observed.rs:500-521` asserts 7 calls for a two-trial scenario. With `n ≥ 2` cuts under `Atomic`, mixed sides add `2·(2^n − 2)` runs for that target set (`:2076-2080`).

Determinism caveats stated in code: plans cover "one input and one (mostly reproducible) schedule; they are not source-level `.await`s and not a proof over all executions" (`src/lib.rs:15-17`). `CurrentThread` reproduces timers/task scheduling but "real I/O, `std::time` and the blocking thread pool are not controlled" (`:116-119`); `MultiThread` uses real time and "Pending counts may vary between runs, so planned cancellations can go unrealized" (`:120-122`). `Reschedule` may re-poll immediately (`:20-23`); `After` is virtual under `CurrentThread` (`:24-25`). A plan is "an instruction to *attempt the same cancellation again*, not a deterministic replay" (`:236-239`); replay repeats this and reports `Unrealized` rather than a different experiment (`src/lib.rs:79-82`, `src/explore.rs:2338-2344`). Gap sites are call-order numbered, and a skipped target can change reachability (`:392-398`); dropping a target before first poll can change control flow and later target numbering (`:2088-2095`, `:2112-2114`). `support::bounded` uses Tokio time, so the paused clock can auto-advance; prefer a real-time flavor for real I/O (`src/support.rs:27-41`). The scenario is a `FnMut` called with a fresh `Ctx`/runtime per run (`:1525-1542`), but captured state persists — build per-run state inside the scenario (`src/support.rs:44-54`). `scenario_timeout` fires only when the scenario yields; one non-returning poll is not interrupted (`:190-196`).

---

## 5. `Settle`, gaps, shutdown

"Cancelled work may finish late (a blocking-pool read, a kernel operation, a detached task). Settling gives it time to do so, so that late effects, unowned results and `Ctx::after_settle` checks can be observed." (`:125-134`). Fields/defaults: `tokio_time: 100ms`, `real_time: 0`, `watchdog: 10s` (`:136-158`). After the scenario returns, `settle()` sleeps `tokio_time` inside the still-running runtime, bounded by the watchdog from a plain thread (`:1506-1512`); under `CurrentThread`, paused time does not auto-advance while `spawn_blocking` runs, so this also waits for in-flight blocking-pool work (`:131-134`). A non-zero `real_time` is a wall-clock wait followed by 16 `yield_now`s ("an observation window, not proof that the work is done", `:141-142`, `:1513-1521`). Then `after_settle` checks run; their errors are invariant violations (`:409-411`, `:1550-1558`). Then the ledger collects leaks; "tasks torn down by runtime shutdown are an accepted form of abandonment" (`:1559-1563`). The runtime closes with `shutdown_timeout(SHUTDOWN_GRACE)`, `SHUTDOWN_GRACE = 1s`, because a `Runtime` drop otherwise "waits forever for `spawn_blocking` work to return" (`:1401-1435`); unwind still closes (`:1430-1435`).

Cancelled target futures are **not spawned, aborted, or awaited**: `Target` drops the inner future at the boundary (`:708-717`); with `race`, the program's `select!` drops or keeps it (`:340-345`, `:810-815`). Detached tasks only get the settle window. **Bounded wait before reuse:** yes, `Ctx::gap(Gap)` (`:388-405`) with `Now`/`Yield(n)`/`Sleep(d)`; `Sleep` is Tokio time, virtual under `CurrentThread` and real under `MultiThread` (`:62-63`, `:118-122`). There is no flavor-independent wall-clock `Gap`. `support::bounded(limit, label, fut)` converts a timeout into a stable `Err` observation (`src/support.rs:19-42`), also Tokio-time.

---

## 6. Existing real-I/O pattern: `dropwise::net::Proxy`

"Loopback-only TCP proxy for cancellation experiments. `Proxy` listens on `127.0.0.1:0` and forwards every accepted connection to a fixed upstream address. It counts the bytes forwarded in each direction, and `residue` reports how many bytes wait in the client socket's receive buffer without reading them." (`src/net.rs:1-6`). "`start_with` can hold every chunk read from the upstream for a configurable real-time delay before writing it to the client. That makes an operation return `Pending` while it waits for the reply without changing the server." (`:8-11`). `net` feature only (`:13`, `Cargo.toml:29-35`).

* `Proxy::start(upstream) -> io::Result<Self>` binds `127.0.0.1:0` and forwards (`:74-79`); `start_with(upstream, ProxyOptions)` adds controls (`:81-103`); `addr()` (`:105-108`).
* `connections() -> Vec<ConnectionStats>`: per accepted connection, client peer address, `to_upstream`, `to_client`; kept for the proxy's lifetime, including disconnected clients (`:26-35`, `:110-127`).
* `residue() -> io::Result<Vec<ConnectionResidue>>`: unread bytes in each **client** socket's receive buffer via `/proc/self/fd` + `FIONREAD`; neither reads nor blocks; 0 if the socket is gone; `Unsupported` outside Linux (`:129-170`, `:317-397`). `max_residue()` is the max over accepted connections, 0 if none (`:172-187`).
* `ProxyOptions { reply_delay }`: wall-clock hold per upstream→client chunk, "meant for `Flavor::MultiThread`"; while held the chunk is not in the client socket, so `residue` does not see it (`:47-61`, `:154-156`). `Drop` signals shutdown and aborts the accept task (`:190-197`).

**Test usage.** `tests/net_proxy.rs` uses plain `#[tokio::test]` (not `explore`): mock upstream, `Proxy::start`, client through proxy; unread reply visible in `residue` (`:102-124`), zero after the client reads it (`:126-146`), non-blocking on a blocking client socket (`:150-170`), byte counters match traffic (`:172-187`). `tests/net_reply_delay.rs` uses `start_with`: a read during the hold times out and the same bytes arrive later in order (`:104-134`); chunk order/counts are preserved (`:138-163`); `residue` is 0 while held and the reply length after forwarding (`:167-201`). No in-crate test combines `Proxy` with `explore`; the repo's `prospective/` tests (outside the packaged crate) do: `sqlx_mysql_proxy.rs:97-107` starts a 50 ms held-reply proxy *inside* the scenario, targets `pool.begin()`, then runs survivors/observer; `lettre_residue.rs:75-114` starts a proxy inside the scenario, targets `send`, sleeps 50 ms, reads `residue`, runs two survivors and reads a server log into `Obs`; its config is `MultiThread { workers: 1 }`, `races: [Immediate, AfterWake]`, `max_runs: 128`, `scenario_timeout: 20s`, `Settle { tokio_time: ZERO, .. }` (`:116-124`).

---

## 7. Cleanest public-API recipe for a socket target; limitations

### 7.1 Recipe

Use a connection **outside** the target, a whole-request future inside `ctx.target`, a per-run proxy whose held reply guarantees the `Pending`, a `Ctx::gap` before reuse, and pack successor value + independent observer into the observed value:

```rust
// Body of: async fn scenario(ctx: Ctx, upstream: SocketAddr) -> Result<Obs, String>
// Sketch: io::Error mapping omitted.
let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: Duration::from_millis(50) }).await?;
let mut conn = TcpStream::connect(proxy.addr()).await?;
let cancelled = ctx.target(async {
    conn.write_all(REQUEST).await?;
    let mut buf = vec![0u8; REPLY.len()];
    conn.read_exact(&mut buf).await?;
    Ok::<_, std::io::Error>(buf)
}).await;
if cancelled.is_none() { /* recovery: drain/discard the abandoned reply */ }
ctx.gap(Gap::Sleep(Duration::from_millis(50))).await;          // bounded wait before reuse
let residue_before = proxy.max_residue()?;                     // independent observer
conn.write_all(SUCCESSOR_REQUEST).await?;
let mut successor = vec![0u8; REPLY.len()];
conn.read_exact(&mut successor).await?;
Ok(Obs { cancelled: cancelled.is_none(), residue_before, successor })
```

Config: `Flavor::MultiThread { workers: 1 }`, real-time `Gap::Sleep`, generous `scenario_timeout`, and `settle.real_time` if late effects matter. Drive one boundary with `Plan { cuts: vec![Cut { target: 0, after_pending: 1, race: Race::Immediate }], ..Plan::default() }` and `replay_observed(&config(), Expect::Atomic, &plan, |ctx| scenario(ctx, upstream))` (`:2372-2414`); or explore all plans and replay `Trial.replay_plan()`. `Expect::Atomic` suits a request whose reply may legitimately be abandoned or consumed; use `Completed` if the guard promises completion, `Undone` if the value must reach the caller (`:76-91`).

### 7.2 Limitations / forced workarounds

1. **Cancelled value and inner future are lost.** `Target<F>` outputs only `Option<F::Output>` and drops the inner future (`:676-677`, `:708-717`); nothing exposes its partial state. If the request future owns the connection, cancellation closes it. Keep the handle in the scenario and pass a borrow (the `direct_handle` shape, `examples/direct_handle.rs:8-16`, `:38-45`), or use `ctx.race` + `select!` and keep `Raced` (it may continue after `Kept`, `:789-806`) and do the drop yourself.
2. **Boundary counting is aggregate, not per-await.** `after_pending` counts every `Pending` of the whole marked future (`:526-536`); there is no "the read's first `Pending`". With the write inside the target (needed for `pre` = "never started"), a pending `write_all` consumes boundary 1. With the write outside, the boundary is precise but `pre` becomes "request sent, reply not read", changing the contract. Splitting write/read into two targets leaves the same semantic gap, and single cuts on the write target produce states that do not correspond to the mechanism under test. Whether a loopback `write_all` contributes a `Pending` at all is UNVERIFIED. Check `Trial.outcomes`/`sites` (`:920-923`) and prefer replay.
3. **No single-run API.** `Plan`, `Cut`, `replay*` are public (`src/lib.rs:103-107`), but replay always reruns the baseline and, for observed, all calibration (`:2345-2361`, `:2372-2414`); no `run_once` is exported. Plain `replay` costs 2 runs (baseline + plan); `replay_observed` costs 6 for a one-cut, one-target plan (baseline + plan + two `pre` + two `post` samples; multi-cut plans add `2·(2^n − 2)` mixed samples). Real-I/O replay is an attempt, not a deterministic rerun (`:2338-2344`).
4. **No flavor-independent wall-clock gap.** `Gap` has only `Now`/`Yield`/`Sleep` (Tokio time) (`:56-64`); `Sleep` is virtual under `CurrentThread`. Waiting for a real reply before reuse forces `MultiThread`, where pending counts vary and cuts may be `Unrealized` (`:120-122`) — a coverage gap, not a pass. `Settle::real_time` applies only after the scenario (`:136-142`).
5. **`ctx.race` pre-calibration can be impossible.** A `pre`-skipped race target never polls its future and its competitor is ready at once; if the program keeps it and polls again (a `select!` loop over `&mut op`), it never completes, so `pre` times out and is a calibration failure (`:2116-2120`). The `preempt` arm must terminate the loop/return to make `pre` producible; `ctx.target` one-shot scenarios avoid this.
6. **Observer must be inside `T`.** No observer parameter or per-field result; the oracle is `T: PartialEq` (`:2123`, `:936-939`). `T` must be `Send + 'static`, and both samples of each side must agree (`:1948-1989`). A unique id, timestamp or address that varies per run makes every plan `Unknown`; normalize it out (`src/support.rs:44-54`) and build server/proxy state inside the scenario for fresh samples (`:46-49`).
7. **`Proxy::residue` blind spot/platform.** During `reply_delay` the reply is held in the proxy, so `residue` is 0, not "no stale reply" (`src/net.rs:154-156`; `tests/net_reply_delay.rs:167-201`). `residue`/`max_residue` are Linux-only (`:163-169`); elsewhere use a server-side observer or `connections()`. `Proxy` never delays client→upstream writes, so it cannot force a write-side `Pending`.
8. **Repeated-site coverage is under-reported.** Later repeated instances have no `CutOutcome`; `is_realized` checks explicit cuts only (`:920-926`, `:952-954`), so a missed repeated boundary is invisible to `unrealized()`/coverage gaps; `Trial.cancelled` is the only count (`:1600-1604`).
9. **Conditional gap registration can drift.** Search does not reject unused `Plan.gaps` entries; only replay does (`:2313-2324`). Gap sites are call-order numbered (`:392-398`), so a skip that changes reachability can shift which wait receives a substitution. Keep gap calls unconditional.

For an offline generator, iterate `report.violations()` (`:1005-1007`) and retain `Trial.plan`/`plan_label`, `sites`, `outcomes`, `verdict`, `comparison`, `replay_plan()` (`:914-975`). Treat an `Expect`-forbidden verdict (`Neither` for all contracts; `Pre` or `Mixed` under `Completed`; `Post` or `Mixed` under `Undone`) as the counterexample, not `Unknown` (undecided: a calibration side was unstable or absent; absent sides also appear in `calibration_errors`) and not `is_clean` alone (true even with zero plans, `Unknown`, `Unrealized`, truncation or unsettled runs; `src/lib.rs:64-69`, `:1101-1117`). Use `Report::diagnostics()` for the per-plan dump (`:1119-1200`).

---

**Sources read:** `src/explore.rs` (all 2445 lines), `src/lib.rs`, `src/models.rs`, `src/support.rs`, `src/obligation.rs`, `src/net.rs`, `tests/observed.rs`, `tests/product.rs`, `tests/net_proxy.rs`, `tests/net_reply_delay.rs`, `tests/explorer.rs`, `tests/semantics.rs` (selected), `examples/quickstart.rs`, `examples/direct_handle.rs`, `examples/limits.rs`, `Cargo.toml`, `docs/using-dropwise.md`, and the prospective proxy scenarios cited above.

*The only UNVERIFIED point is the environment-dependent loopback `write_all` behavior noted in §7.2 item 2; everything else is read directly from the cited source or tests.*
