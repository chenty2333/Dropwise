# Round 3 exploration brief (working notes, not a frozen protocol)

Started 2026-10-21. This round expands real client/service coverage after the
first SMTP/IMAP/BOLT exploration. It is **unfrozen exploration**, not formal
Phase 4 data: do not fill Phase 4 tables, do not claim preregistration, and do
not re-label a previously touched target as blind.

Read first (in this order):

- `prospective/phase4-exploration/RESULTS.md` (rounds 1–2, method lessons)
- `docs/writing-scenarios.md` §2.5, §3, §10, §11
- `docs/choosing-targets.md`
- `research/theory/THEORY.md` §2–§5
- the relevant `prospective/phase4/targets.md` entry

Do **not** read `research/linux/PRIVATE-NOTES.md` or any `PRIVATE-NOTES.md`.

## Hard rules

1. **No commits, no pushes, no issues/PRs/emails.** The lead commits at the end.
2. **Do not overwrite historical results or logs.** New directories and new
   `logs/run.XXXXXX/` only. Never edit `RESULTS.md`/`RESULTS-round2.md` of the
   previous rounds.
3. **One target = one independent root cause**, not one violation/plan count.
   Report executions/plans separately.
4. **Do not use `Ok` as correctness.** Record concrete values, protocol state,
   and an independent observer. A cancelled operation may legitimately complete:
   compare against `pre` and `post` calibration (`Expect::Atomic`), not only
   `post`.
5. **Observer must bypass the tested pool/handle/proxy** (separate connection,
   server log, REST mailbox view, etc.).
6. **Same object**: the survivor must touch the same key/row/mailbox/node as the
   cancelled target. Different-object survivors are controls, not substitutes.
7. **Pool size / concurrency = 1** where the API allows. Record actual reuse and
   reconnect; do not block legal self-healing, but do not silently reconnect
   before the survivor.
8. **Pending mechanism**: one mechanism per target, all levels share it. Prefer
   a server-side delay primitive; otherwise `dropwise::net::Proxy` with
   `reply_delay` (real time, `Flavor::MultiThread`). Record it.
9. **Contract first**: write down what success/error allow before judging.
   Search the public docs for cancel/timeout/drop/abort/consistency statements.
   A documented "not guaranteed" makes the case contract-external.
10. **Zero result**: write "no violation observed in the listed version,
    scenario and realised cancellation points" plus coverage numbers. Never
    "clean" or "proven safe".
11. **Clean up** every container/VM/service you start. Only bind loopback.
12. **Timebox** a family: after ~45–60 min of environment blocking, record the
    concrete blocker and move on. Do not stall the whole batch.

## Experiment shape (Rust)

Copy the existing pattern (do not invent a new engine):

- `prospective/phase4-exploration/clients/bolt/round2/shared/round2.rs`
- `prospective/phase4-exploration/clients/imap/round2/src/main.rs`
- `prospective/support/ladder.rs` for `LADDER*` lines

Four levels, same setup/target/Pending/Config, 3 reps:

- B1: cancel target, stop; panic/timeout/obligation only.
- B2: same handle/pool, same arguments retry; only "not an error".
- B3: same handle/pool, different operation of the same kind; only "not an error".
- M1: `explore_observed(Expect::Atomic)`, survivor does a side effect on the
  **same object**, observer bypasses the tested unit; observation contains the
  concrete return value plus observer state.

Config: `Flavor::MultiThread { workers: 1 }`, `races = [Immediate, AfterWake]`,
`max_cancellations = 1`, `max_runs = 64`, `scenario_timeout = 20 s`,
`Settle { tokio_time: ZERO, real_time: ZERO, watchdog: 10 s }`.

Also before the ladder: one uncancelled run, one reuse/reconnect diagnostic,
pre/post calibration, observer-visible effect. Use `bounded()` (2 s) around
calls that can hang so a hang becomes a recorded value.

For generated target×successor combinations, `prospective/support/combine.rs`
may be used, but a hand-written level ladder is enough for a first contact.

## Experiment shape (Python)

Reuse `research/python-retro/dropwise_py/` **read-only**. One `Scenario`
subclass per target, fresh instance per run:

- `setup`: pool/handle with limit 1, independent observer, unique key/table,
  warm-up if needed.
- `target`: the state-establishing round trip (transaction begin, checkout,
  session/use, mailbox select).
- `same` / `different`: B2/B3.
- `survivor`: same object side effect (and same-connection readback).
- `observe`: independent connection/server view.
- `close`: bounded cleanup; never swallow the runner's `CancelledError` in a
  way that keeps the scenario alive.

Engine: `count_yields`, `level_b1`, `level_retry`, `explore_observed`,
`print_report` from `dropwise_py`. `Expect.ATOMIC`. Record `yields`, realised
plans, pre/post, `Unknown`, hard-deadline/incomplete separately.

Use `uv venv` + `uv pip install` outside the repo (e.g.
`~/dropwise-py-work-r3/`), pin the versions from `targets.md`. Record
`python -V`, package versions and server image digest.

## Triage and fix

For each candidate violation:

1. Re-run; exclude harness/observer/timing misses.
2. Shrink to the plan/stage; check the actual protocol/server trace.
3. Read the client's cancellation/recovery implementation **only now**.
4. Classify: confirmed client bug / contract-external / harness error /
   unresolved. A confirmed case needs a user-visible consequence **and** a
   patch/config contrast that removes it.
5. Search upstream issues/PRs read-only (crate name + cancel/reset/pool/reuse).
   Record known vs new; do not contact upstream.
6. If fixing: dependency-copy patch (`[patch.crates-io]` or a copied build),
   never the Dropwise core; run original vs fixed, plus normal/rejection paths.
7. Keep the patch minimal and explain which layer it repairs. If recovery only
   fixes operation availability but not metadata, say so.

## Required artifacts per family

```
clients/<family>/
  SCENARIO.md            operations, reuse unit, pending, observation, contract
  Cargo.toml / src|tests / run-*.sh
  RESULTS-FRAGMENT.md    versions, table, violations, triage, cost, limits
  logs/run.XXXXXX/       raw output (not overwritten)
```

`RESULTS-FRAGMENT.md` must include: service image digest; client/manager
versions; reuse/reconnect evidence; per-level plans/violations/unrealized/
baseline errors; concrete observation values for violations; pre/post equality
note; known/new; patch contrast (if any); what was not covered.

Python equivalents under `prospective/phase4-exploration/python/<batch>/`.

## Report back to the lead

Send a short message with: directories, families completed/blocked, confirmed
issues + classification, counts (executions vs root causes), and the single most
important counterexample or new shape. Do not paste whole logs.
