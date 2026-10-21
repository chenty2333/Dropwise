# Gel 0.11.0 — four-level cancellation ladder (B1/B2/B3/M1)

**Status: complete for the requested Gel ladder; no violation found.** 96 cut
trials across 3 repetitions (8 cut plans per level per rep, 24 per level, plus
one uncancelled baseline per level/rep) with an independent observer, concrete
`(v, marker)` values and pre/post calibration. Violations 0, unknown 0,
unrealized 0, unsettled 0, baseline errors 0. The probe in
`RESULTS-FRAGMENT.md` is superseded for ladder purposes, not deleted.

Run: `logs/run.Zlfofp/` (new directory; `run.20261021T125445Z` untouched).
Container `dw-gel-ladder-574347` was removed by the runner's `EXIT` trap; no
test service remains. No Dropwise core change, no dependency patch, no
upstream contact, no push, no other agent's logs touched.

## Exact versions / service

| item | value |
|---|---|
| date (UTC) | 2026-10-21T14:19–14:20Z |
| repo HEAD | `a4af527f956d5348b671dc8a5c1387080fdf1b00` |
| rustc / cargo | 1.98.0 (88d9e12ae 2026-08-18) / 1.98.0 (797e8a9bc 2026-08-05) |
| client | **gel-tokio 0.11.0** (`Client` pool, `max_concurrency(1usize)`) |
| client deps | gel-dsn 0.2.16, gel-protocol 0.9.2, gel-stream 0.4.5, tokio 1.54.0 |
| derive_more | lock pin **2.0.1** (2.1.1 breaks gel-stream 0.4.5 on rustc 1.98; not re-tested this round) |
| server image | `geldata/gel:7` = `geldata/gel@sha256:b7270b0973da6950d01ae0d578c6d38cd8d87fabdd6c4b75a09b74291ad6f3a8` |
| server | Gel **7.1+08db576**, `GEL_SERVER_SECURITY=insecure_dev_mode` (Trust auth, self-signed TLS) |
| client TLS | `TlsSecurity::Insecure`, user `admin`, branch `main` |
| port | 127.0.0.1:32864 → container 5656 (ephemeral, loopback only) |
| build | debug (`p4e-gel`), `cargo build --offline`; build log in run dir |
| pending | `dropwise::net::Proxy`, `reply_delay = 50 ms` |
| config | `Flavor::MultiThread { workers: 1 }`, races `[Immediate, AfterWake]`, `max_cancellations=1`, `max_runs=64`, `scenario_timeout=20 s`, `Settle { 0, 0, watchdog 10 s }` |
| variant label | `gel` (`SCAN_VARIANT=gel`) |

### Pooled vs direct (only one mode implemented)

gel-tokio 0.11.0 exposes **no public direct connection handle**: `raw` is a
private module, `Client` is the only public handle and internally owns a pool.
`max_concurrency(1usize)` makes the pool hold at most one connection at a time.
It is not a persistent direct handle: a request cancelled mid-flight leaves the
pooled connection in `Mode::Dirty`; `PoolConnection::drop` does not return an
inconsistent connection to the queue, so the next operation opens a fresh
connection. The ladder therefore has **one mode**, labelled pooled/single
connection, and the probe's `proxy_connections=2` after a cut is the expected
reconnect. A true direct mode does not exist in this version.

## Method

- **Setup (not a cut):** independent observer `Client` direct to the server
  (`ensure_connected`), create `DwGel` if absent, insert a process-unique
  `k` row `(v=0, marker='seed')`; tested `Client` through the proxy, warmed by
  `ensure_connected`. Every scenario run/calibration gets fresh state via
  `dropwise::support::unique`.
- **Target:** `Client::transaction` whose body runs
  `update DwGel filter .k = $0 set { v := $1, marker := $2 } select (u.v, u.marker)`
  with `v=1, marker='target'`. It is the only `Ctx::target`; the whole
  transaction future is the cancellation target.
- **Cut sites (inferred from `#0@N` plans and observed effects):** transaction
  starts lazily, so the target future has four `Pending` boundaries: `@1`
  START TRANSACTION reply, `@2` update Parse/Describe reply, `@3` update
  Execute/result reply, `@4` COMMIT reply. Both races run at each site, giving
  the 8 cut plans per level per rep.
- **B1:** cancel target, stop; judged by `explore` (panic/liveness/leak).
- **B2:** cancel target, retry the same transaction once on the same `Client`.
- **B3:** cancel target, run a different operation once (plain `select (v,marker)`).
- **M1:** cancel target; independent observer reads `before`; survivor
  transaction writes `v=7, marker='survivor'`; same-client `own` read; observer
  `visible` read. `Obs` is judged by `explore_observed(Expect::Atomic)` with
  automatic two-sample pre/post calibration; `calibrate-pre` / `calibrate-post`
  additionally print two manual samples per side per rep.
- All post-cut operations are bounded at 2 s. Probe/setup errors are separate
  from cut outcomes. No `Ctx::gap`, no repeats, one cut per run.

## Per-level counts (3 reps)

`LADDER` labels its trial list `plans`; each level/rep has **8 cut-plan trials**
and **1 uncancelled baseline run** that is not a trial. Aggregate over 3 reps:

| level | cut plans/trials | realized cuts | violations | unknown | unrealized | unsettled | baseline errors | concrete outcome |
|---|---|---:|---:|---:|---:|---:|---:|---|
| B1 | 24 / 24 | 24 | 0 | 0 | 0 | 0 | 0 | 24 cancelled; baseline 3/3 completed |
| B2 | 24 / 24 | 24 | 0 | 0 | 0 | 0 | 0 | 24/24 retry `Ok((1,"target"))`, observer `[(1,"target")]` |
| B3 | 24 / 24 | 24 | 0 | 0 | 0 | 0 | 0 | 24/24 read `Ok`; 21 `[(0,"seed")]`, 3 `[(1,"target")]` |
| M1 | 24 / 24 | 24 | 0 | 0 | 0 | 0 | 0 | **Pre 21, Post 3**, Mixed 0, Neither 0, Unknown 0 |

All 12 level/rep runs: `exhaustive=true`, `baseline_targets=1`,
`unsettled=0`, and no `GEL-GAP` line (which would also report a baseline-settle
or truncation gap), no `LADDER-ERROR`, no `GEL-VIOLATION`. Per-rep M1 verdicts
are identical: `pre=7 post=1 mixed=0 neither=0 unknown=0`, with the single Post
trial always `#0@4:AfterWake`.

Manual calibration: 6 command invocations (pre/post × 3 reps), 2 samples each;
6/6 `GEL-CAL-OK identical=true`. Automatic M1 calibration produced no
`calibration_errors` and no `Unknown`.

## Concrete evidence

Preflight (uncancelled, through proxy): target `(1,"target")`; observer,
same-client `own` and second read all `[(1,"target")]`;
`proxy_connections=1`. So setup is stable, the update is observable, and the
normal path reuses one connection.

| stage | pre calibration (2/2 samples) | post calibration (2/2 samples) |
|---|---|---|
| observer `before` | `[(0,"seed")]` | `[(1,"target")]` |
| survivor `write` | `Ok((7,"survivor"))` | `Ok((7,"survivor"))` |
| same-client `own` | `[(7,"survivor")]` | `[(7,"survivor")]` |
| observer `visible` | `[(7,"survivor")]` | `[(7,"survivor")]` |
| proxy connections | 1 | 1 |

The M1 two sides differ concretely in `before`, so `Expect::Atomic` can
distinguish "never happened" from "completed"; the survivor values are identical
and agree between same-client and independent observer.

**No violation counterexample exists in this run.** The concrete
cancellation-ambiguity case is a Post trial:

```
plan=#0@4:AfterWake cancelled=1 verdict=Post
M1-TARGET run=1 cancelled=true
GEL-OBS before=Ok([(1,"target")]) write=Ok((7,"survivor"))
        own=Ok([(7,"survivor")]) visible=Ok([(7,"survivor")]) proxy_connections=2
```

This is the COMMIT reply wake: the server had already committed the target
transaction when the caller's future was dropped, so the independent observer
saw `(1,"target")` after the cut. Under `Expect::Atomic` `Post` is allowed; the
survivor then wrote `(7,"survivor")` and both readers agreed. The matching B3
observation is the 8th (`@4:AfterWake`) read returning `[(1,"target")]` in each
rep (3/24 total); all other B3 reads saw `[(0,"seed")]`.

At `@1`–`@3` and `@4:Immediate` (the seven Pre trials) the target never
committed: the observer saw seed, the pooled connection was discarded, the
survivor opened a fresh connection (`proxy_connections=2`; one connection is the cut one, one the
survivor), and the survivor was clean. The server log contains 12
`postgres: canceling statement due to user request` lines for the update query,
consistent with connection-close abort of the open transaction, and no
survivor/observer divergence.

## Contract / upstream status (search only, no contact)

- `Client::transaction` documents only closure outcomes: `Ok` commits, `Err`
  retries/aborts; it says nothing about dropping or cancelling the outer future
  ([gel-tokio 0.11.0 client source](https://docs.rs/gel-tokio/0.11.0/src/gel_tokio/client.rs.html)).
- `RawTransaction` (feature `unstable`, not enabled here) explicitly documents
  drop-time rollback, but is a different API and does not give `Expect::Atomic`
  semantics for the retrying `transaction` future.
- Source read: `Transaction` has no `Drop`; `RetryingTransaction::drop` sends it
  through a oneshot that a cancelled outer future no longer receives; the
  transaction and its dirty `PoolConnection` are dropped. `PoolConnection::drop`
  returns the connection only when `is_consistent()` (`Mode::Normal`), so a
  cancelled request's connection is discarded. At `@4:AfterWake` the COMMIT was
  already processed, but its reply was unconsumed, so the connection was still
  discarded and the effect remained committed.
- Web search surfaced the client docs and
  [standalone transactions #369](https://github.com/geldata/gel-rust/issues/369);
  no report specifically describing this commit/cancel boundary was found.
  GitHub issue bodies could not be fetched from this environment
  (`github.com` resolves to a non-public IP), so this is a search-level status,
  not a complete upstream triage. No upstream contact was made.

**Classification:** documented recovery for closure `Ok`/`Err`; **silent for
outer-future cancellation**. Observed behavior is safe fail-fast on the
connection (dirty connection discarded, no stale reply handed to the survivor,
no wrong value), but the transaction outcome at `@4:AfterWake` is
**committed-despite-cancelled** and therefore unknown to the caller. That is an
inherent commit-boundary ambiguity, not a stale/wrong effect and not a confirmed
upstream bug. **Caller obligation:** do not treat cancellation/timeout of
`Client::transaction` as proof of rollback; reconcile the record (or make the
transaction idempotent) before retrying or reporting failure.

## Fix contrast

No violation, so no dependency-copy/worktree patch was made and no
original/fixed regression is claimed. A drop-spawned `ROLLBACK` (the
`RawTransaction` pattern) would not remove the `@4:AfterWake` window: once the
server has committed, rollback is no longer possible. The only cheap
contract-level improvement would be documentation of the cancellation outcome;
a true fix needs an explicit outcome handle or idempotent reconciliation. This
remains a candidate documentation/contract gap, not a confirmed new upstream
bug.

## Coverage / gaps

- One target shape only: pooled `Client::transaction` with one update+select;
  no `transaction_raw`, no explicit `commit`/`rollback`, no closure-`Err`
  recovery contrast.
- One connection mode only (`max_concurrency(1)`); no multi-connection pool,
  no pool-checkout contention, no exhaustion, no waiter cancellation.
- One cut per run; no `repeat_at_site`, no `gap_choices`, no multi-cancellation.
- B1/B2/B3 judge liveness/`Ok` only; their concrete values are printed but not
  atomicity-judged (M1 provides that).
- Settle is zero (`tokio_time=0`, `real_time=0`); late effects after scenario
  end are not observed. The commit-after-cancel effect was visible immediately
  and is not a late-effect claim.
- No handshake/TLS cancellation, no cancellation during connect, no other
  branch/database, no load, no wire-level byte trace; server log is the only
  server-side evidence.
- The four pending boundaries are inferred from plan labels and observed
  effects, not from a protocol trace; only the COMMIT boundary is distinguished
  concretely by the Post observation.

## Artifacts

- `src/main.rs` — ladder binary (`preflight`, `calibrate-pre|post`, `ladder <rep> <level>`).
- `run-ladder.sh` — disposable container, build, 19 phases, count digest, teardown.
- `logs/run.Zlfofp/` — `environment.txt`, `build.log`, `timings.txt`,
  per-phase `stdout.log`/`stderr.log`, `container.log`, `counts.txt`.
- Total ladder runtime 93.2 s (19 phases).
