# Gel 0.11.0 — round 3 results fragment (probe, not a ladder)

**Status: partial — environment proven, one short cancellation probe run, no
four-level ladder.** 3/3 probe reps cancelled the first transaction round trip,
the observer saw **no side effect** (`(0,"seed")`), and the **same client**
completed a survivor transaction `(7,"survivor")` that both the client and the
independent observer then read. This is a single-point negative, not a
replacement for B1/B2/B3/M1; no violation is claimed or ruled out.

Run: `logs/run.20261021T125445Z/` (`environment.txt`, `probe.log`). Container
`dw-r3-gel-424033`, server Gel 7.1+08db576.

## Exact versions

| item | value |
|---|---|
| rustc | 1.98.0 (88d9e12ae 2026-08-18) |
| gel-tokio | **0.11.0** (`Config::max_concurrency = Some(1)`) |
| server image | `geldata/gel@sha256:b7270b0973da6950d01ae0d578c6d38cd8d87fabdd6c4b75a09b74291ad6f3a8` (`geldata/gel:7`) |
| server | Gel 7.1+08db576, insecure dev mode (Trust auth), TLS insecure on the client |
| port | 127.0.0.1:32840 → 5656 |
| build | needs `derive_more 2.0.1` pin; 2.1.1 breaks `gel-stream 0.4.5` on this rustc |

No dependency patch (only a lockfile pin); client source was not modified.

## Probe (3 reps, `probe.log`)

Target: `Client::transaction` whose body issues one update+select round trip;
cut with `timeout(10 ms)`; Pending via proxy `reply_delay=50 ms`; one tested
`Client` (`max_concurrency=1`) through the proxy, one observer `Client` direct.

| rep | cancelled | observer after cancel | stale-client survivor | own read | observer read | proxy conns |
|---|---|---|---|---|---:|---:|
| 0 | true | `(0,"seed")` | `Ok((7,"survivor"))` | `(7,"survivor")` | `(7,"survivor")` | 2 |
| 1 | true | `(0,"seed")` | `Ok((7,"survivor"))` | `(7,"survivor")` | `(7,"survivor")` | 2 |
| 2 | true | `(0,"seed")` | `Ok((7,"survivor"))` | `(7,"survivor")` | `(7,"survivor")` | 2 |

Interpretation, bounded by the probe: the cancellation landed before the
transaction's side effect became visible; the pooled client did not hand a
poisoned/aborted transaction connection to the survivor; the survivor's own
reply and an independent observer agree. This is consistent with the client
aborting the transaction on drop, but the probe did **not** assert rollback or
inspect the transaction state, so that mechanism is not established here.

## Levels / calibration / counts

**Not run.** There is no B1/B2/B3/M1 table for Gel, no `pre`/`post` calibration,
no generated target×successor cases, and no per-level counts. The single probe
has 3 realized cancellations and 0 concrete-value violation checks under
`Expect::Atomic` (none were executed).

## Recovery contrast

Documented recovery for a transaction is abort/rollback (or retrying
`transaction`); the probe only observed that the record stayed at seed after the
cut. No explicit rollback call and no stale-vs-fresh contrast were run. Not
covered.

## Method-increment question (B2/B3 vs M1)

Cannot be answered for Gel: B2/B3/M1 were not executed. The probe's survivor
values were concrete `(7,"survivor")` and agreed with the observer, but that is
one arm of M1-like evidence, not the level comparison.

## Triage / next steps (if this family is resumed)

1. Implement the intended four-level ladder from `SCENARIO.md` and a 3-rep run
   with pre/post calibration and reuse evidence (session/pool identity,
   `max_concurrency=1`).
2. Add the generated adapter (`combine.rs`) with target = transaction or plain
   query, successors update / update+read / read.
3. Add an explicit rollback/recovery contrast and assert that the aborted
   transaction's connection is reusable, with an independent observer.
4. Record the `derive_more` pin in the runner environment; it is required for a
   reproducible build on rustc 1.98.

## Coverage gaps

Everything except the single 10 ms transaction cut: no B1/B2/B3/M1, no
calibration, no generator, no recovery contrast, no multi-worker, no pool
exhaustion, no rollback assertion, no source read, no upstream search.
