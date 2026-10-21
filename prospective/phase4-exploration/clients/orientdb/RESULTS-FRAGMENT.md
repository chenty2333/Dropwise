# OrientDB 0.6.0 — round 3 results fragment

**Result: no cancellation violation observed** in orientdb-client 0.6.0 with
`SessionPool` size 1 against `orientdb:3.0.23`, for the realized cancellation
points below. 30/30 main ladder plans and 6/6 held-session plans completed with
zero `violations()`, zero `unrealized()`, zero `unsettled()`; all 13 generated
target×successor cases also had zero violations. This is a bounded negative, not
a proof: the pool holds **two physical connections** (see reuse), so a stale
reply can be masked by stream selection, and only one class/record shape and one
Pending mechanism were exercised.

Run: `logs/run.CI4tHK/` (fresh container `dw-r3-ori-433583`, fresh database
`dw_r3_433583_125230`). Wall time **88 s** for 24 level runs + 3 diagnostics +
13 generated cases. `run-round3.sh` removes the container on exit; the old probe
container `dw-r3-ori-411588` was removed separately.

## Exact versions

| item | value |
|---|---|
| rustc / cargo | 1.98.0 (88d9e12ae 2026-08-18) / 1.98.0 |
| orientdb-client | **0.6.0**, features `tokio-runtime` (`aio`) |
| mobc | 0.7.3 |
| Dropwise | 0.1.0 (workspace) |
| server image | `orientdb@sha256:bea416ec004e609c4a44609808f70a355cb61193c01f65e3ac1f73d1a692d668` (`orientdb:3.0.23`) |
| database | `dw_r3_433583_125230`, created `POST /database/<db>/plocal` → HTTP 200 |
| ports | binary 32841, HTTP 32842, loopback ephemeral |

## Setup / preflight (before any ladder)

`preflight` (`diagnostic-preflight.log`): target returned `(1, "target")`,
survivor wrote and read `(7, "survivor")`, independent observer saw
`(7, "survivor")`, pool `(size=1, idle=1, used=0, max=1)`, proxy 2 connections.
Calibration for both Pre and Post was accepted (`baseline_targets=1`,
`baseline_errors=0`, `calibration_errors=0` in every level below).

Reuse (`diagnostic-reuse.log`): five sequential `(write, read)` requests through
one pooled session; `session_id=389` stayed constant; pool stayed
`(1, 0, 1, 1)`; proxy connections stayed at **2**. So the logical session is
reused, but there are two stable physical TCP streams (Open/query layering).
`SessionPool::max=1` bounds one *logical* session only; `ConnectionOptions::
pool_size(1)` is not reflected in the physical count. Recorded as a property of
the version, not worked around.

## Ladder, 3 repetitions, zero violations

Config: `MultiThread { workers: 1 }`, races `[Immediate, AfterWake]`,
`max_cancellations=1`, `max_runs=64`, `scenario_timeout=20 s`,
`Settle { 0, 0, watchdog 10 s }`. Pending: proxy `reply_delay=50 ms`.
Main target = checkout (`SessionPool::get`) + one complete script request;
held-session target = the script only, on a session held across the cut.

| level | plans/rep ×3 | violations | unrealized | unsettled | pre/post/other |
|---|---|---:|---:|---:|---|
| B1 main (cancel+stop) | 10 ×3 = 30 | 0 | 0 | 0 | — |
| B2 main (same args, `Ok` only) | 30 | 0 | 0 | 0 | 30/30 retries `Ok((1,"target"))` |
| B3 main (read, `Ok` only) | 30 | 0 | 0 | 0 | 24 reads `(0,"seed")`, 6 `(1,"target")`, all `Ok` |
| M1 main (`Atomic`, concrete) | 30 | **0** | 0 | 0 | **pre=29, post=1**, mixed/neither/unknown=0 |
| S-B1 held session | 6 | 0 | 0 | 0 | — |
| S-B2 held session | 6 | 0 | 0 | 0 | 6/6 `Ok((1,"target"))` |
| S-B3 held session | 6 | 0 | 0 | 0 | 6/6 `Ok((1,"target"))` |
| S-M1 held session | 6 | **0** | 0 | 0 | **pre=3, post=3**, other=0 |

M1 observed values were all internally consistent: in every realized plan
`own == visible == write == (7, "survivor")` and `before` was either
`(0, "seed")` (target had no effect) or `(1, "target")` (target completed before
cancellation), which `Expect::Atomic` accepts. No mixed state, no reply from the
cancelled target attributed to the survivor. The single `post` in main M1 rep1
(and the 3 post plans in S-M1) are exactly the cases where `before` already
equalled the post state; they are still accepted atomically.

`S-B2` in this run realized 2 plans/rep with 0 unrealized; an earlier standalone
run had 8 plans with 6 unrealized. The runner's value is authoritative.

## Generated cases (same method, `GEN` prefix)

`tests/generate.rs`, `combine.rs` Adapter, 13 cases (target
`update(primary,1)` / keyless `checkout`; successors `update`, `update+read`,
`read`; same/different object and same/different payload where the domains
exist). One run (`generate.log`): **13/13 applicable, 0 skipped, 0 unrun, 0
violations**, 124 realized plans:

- 9 update-target cases: 10 plans each, all `post=10` (target finished before
  the cut) and 0 violations;
- `update(primary,1) -> read(primary)`: 10 plans, `pre=8 post=2`, 0 violations.
  Its diagnostics show the expected two legal values
  (`(0,"seed")` vs `(1,"target")` in `own_read`/`visible`), not an attribution
  error;
- 3 keyless-checkout cases: 8 plans each, `post=8`, 0 violations.

Caveat recorded from that run: the generated `Obs` does not include the
pre-target observer snapshot, so its earlier 9 update-target cases were
pre/post-degenerate (all `post`) even though the values were checked. The
ladder's M1 `Obs` does include `before`, and that is where the 29 pre / 1 post
split comes from.

## Recovery contrast

`OSession::close(self)` exists but is a no-op for a pooled session; the pool
documents no cancel/reconnect operation. The contrast test uses
`timeout(5 ms, target)` and then (a) the stale pool and (b) a fresh
`OrientDB`+`SessionPool` through the same proxy:

3/3 reps: cancelled=true, observer `(0,"seed")`, pool `(0,0,0,1)`,
proxy_connections=1 (the 5 ms cut fired **before** the checkout reached the
server), stale survivor `Ok((7,"survivor"))`, fresh survivor
`Ok((7,"survivor"))`, observer `(7,"survivor")`. Since the stale pool never had
a request in flight, this is **not** a recovery contrast yet; the documented
reconnect path works, but no poisoned state was produced to recover from. The
ladder's realized cuts (50 ms delay) are the evidence that matters here.

## Triage / known-new

- Zero M1 violations in 36 reviewed plans; no upstream issue search was run for
  this family (zero result, so nothing to triage as known/new).
- The two-physical-connection layering is a **contract-external / measurement**
  observation, not a client bug: 0.6.0's public pool bounds sessions, not
  streams. It limits what the harness can attribute to a single stream.
- B2 `Ok` values and B3 `Ok` values never hid an M1 concrete-value mismatch:
  B3's legal two-value distribution (seed/target) is exactly what Atomic
  accepts, and M1 had 0 violations. **B2/B3 would not have passed a case M1
  failed, because M1 failed no case**; per-level counts are kept separate above.

## Coverage gaps / not covered

- One class `DwOri`, one record per case, integer+string payload only; no
  transactions, links, multi-page results, live queries or server-side delays.
- One Pending mechanism (proxy reply delay 50 ms) and two races; workers=1.
- No network reset/kill, no pool eviction/`close` mid-flight, no max_size>1,
  no M1-residue, no upstream source read of the cancellation path (not needed:
  zero violations), no dependency patch.
- Cannot prove one physical connection (see reuse); no claim of single-stream
  isolation.
- The recovery contrast needs a cut that actually reaches the wire (the 5 ms
  timeout did not), or a Dropwise-selected violating plan; neither was achieved
  before the timebox.

## Files

`SCENARIO.md`, `Cargo.toml`, `src/lib.rs`, `tests/orientdb.rs`,
`tests/session.rs`, `tests/generate.rs`, `tests/support/generate_adapter.rs`,
`run-round3.sh`, `logs/run.CI4tHK/` (24 level logs, 3 diagnostics,
`generate.log`, `environment.txt`, `cost.txt`).
