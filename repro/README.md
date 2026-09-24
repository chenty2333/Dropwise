# Paired reproductions

Each case runs one Dropwise scenario against the code **before** and **after** the real fix.
A case passes when the buggy variant shows a violation and the fixed variant is clean.
`./run_all.sh` re-runs everything (version-switched cases use `cargo update --precise`).

Only defects confirmed by both independent annotators (`survey/labels/human/relevant.csv`)
are used. Evidence levels (codebook v3, field `reproduced`):

- **executed**: the original library / program at pinned versions;
- **modelled**: a reduced model copying the code shape before and after the fix, for
  large systems whose code cannot be exercised in isolation.

| Case | Level | Buggy → fixed | Found without injection? | Result |
|---|---|---|---|---|
| tokio#6877 `write_all`/`read_exact` in `select!` | executed | reporter's loop → both ops kept alive (tokio 1.40.0) | **yes**: the program's own `select!` already cancels `write_all` | baseline fails; fixed clean |
| hyper#3995 dispatcher dropped mid-body | executed | hyper 1.9.0 → 1.10.0; confirmed at 156a6f6 → b7a679b (fix commit) | no | 4/8 plans: body ends `Ok` after 5/10 bytes; fixed clean |
| tokio#7979 cancelled io_uring `open` leaks fd | executed | tokio 1.51.0 → 1.52.0; confirmed at ad8c59a → c791213 (fix commit) | no | 1 fd leaked per plan, visible **only after settling**; fixed clean |
| pingora#931 RTCache lock kept after cancelled lookup | executed | pingora-memory-cache 0.9.0 (latest release) → PR #948 head | no | 2/2 plans: later caller hangs (liveness, via scenario watchdog); fix clean |
| databend#20020 flag set before awaited finish | modelled | shape before/after #20021 | no | violation at the on_finish boundary; fixed clean |
| materialize#38577 placeholder across await | modelled | before/after fix | no | later `into_result` "panics"; fixed clean |
| qdrant#9665 `local.take()` then await | modelled | before / fix proposed in #9666 | no | shard slot left `None`; fixed clean |
| risingwave#3909 dedup leader fetch in-future | modelled | before/after #3911 (spawned fetch) | no | follower stranded; fixed clean |
| tokio#3825 `Notified` recreated in a `select!` loop | executed | reporter's loop → maintainer's pinned `Notified` (tokio 1.53.1) | no | hangs only when the competitor wins **after** the notification arrived (`Race::After(5s)`); `Immediate`, `Reschedule(1)`, `After(1s)` clean; fix clean |
| redis-rs#851 `XREAD BLOCK 0` dropped in `select!` | executed | reporter's program (redis-rs 0.23.3, Redis 8 in docker) → finite block | **yes**: the program's own `select!` drops the in-flight XREAD | baseline hangs: the server keeps blocking after the future is dropped; fixed clean (3 plans unrealized: real network) |
| neon#12345 batch leader cancelled mid-batch | modelled | leader in-future / leader work runs to completion | no | violation only at the mid-batch boundary, not while waiting for the lock |

## Observations

Selection for the second round of executed cases (2026-09-24): app-layer, runnable in the
original program, and a mechanism not covered by the first four. tokio#3825 (a lost
edge-triggered notification) and redis-rs#851 (work continuing on the server after the
future is dropped) qualify. tokio#5285 was considered and dropped: its mechanism (a
length-prefixed read torn by a `select!` timer) is the same as tokio#6877. Among the 97
verified defects, the large app-layer systems (Materialize, Neon, Qdrant, RisingWave,
omicron) cannot be run in isolation; the runnable app-layer cases are reporters' programs.

- In 9 of 11 cases the defect appears **only** under injected cancellation; the uncancelled
  baseline is clean. tokio#6877 and redis-rs#851 are the exceptions: the reporters' programs
  cancel on their own.
- tokio#3825 needs control over *when* the competitor wins: the notification is lost only if
  the competitor wins after it arrived. The race parameter (5 s, against the report's 4 s
  notification) was chosen after reading the report.
- tokio#7979 needs the settle phase: the fd leaks when the kernel completes the open after
  the future is gone.
- pingora#931 needs the scenario watchdog: without it the exploration itself would hang.
- While writing the fixed variant of tokio#6877, a first version kept only the writer alive.
  Dropwise reported torn reads, because `read_exact` is not cancel safe either (as the
  maintainer had said). The committed fixed variant keeps both operations alive.
- The pingora#931 defect is present in the latest release (0.9.0, 2026-09-09); the fix PR
  is still open. `pingora-931/upstream-test/` is a standalone reproduction without Dropwise
  (plain tokio, one test): it fails on 0.9.0 and passes on the #948 head. The #948 branch is
  based on 0.8.0, so the Dropwise pair compares 0.9.0 with a 0.8.0-based fix; this is not a
  confound here: the crates.io sources of pingora-memory-cache 0.8.0 and 0.9.0 are identical
  (all of `src/`, compared file by file), the #948 head differs from them only by the PR's own
  diff (138 changed lines in `read_through.rs`), and #948 is mergeable into main (checked
  2026-09-24).

## Limits

- The contracts (invariants, obligations) were written **after** reading each report. These
  results show that Dropwise can express and reproduce known defects; they do not show that
  it finds unknown ones (that needs a prospective study with contracts fixed in advance).
- `modelled` cases depend on the model's fidelity to the original code; each model cites the
  fix it copies.
- `biased;` is used in `select!` where the original relies on random branch order, to make
  runs reproducible.
- Large app-layer systems (Materialize, Neon, Qdrant, RisingWave, Databend) could only be
  modelled; building and driving them in situ was out of reach for this round.

## Validity checks (2026-09-24)

Prompted by finding that the pingora#931 fixed variant was built from a 0.8.0-based branch,
every pair was checked for differences other than the fix itself.

**Resolved versions.** Every lockfile was compared with the versions stated here. One
statement was wrong and is corrected above: tokio#3825 ran on tokio **1.53.1** (resolved from
`"1.40"`), not 1.40. The results show the defect on 1.53.1; the reporter's 2021 version was
not tested. All other stated versions match the lockfiles; cases that state no tokio version
run on 1.53.1.

**Release ranges vs fixing commits.** Two pairs switch between releases, and both ranges
contain other relevant changes: hyper 1.9.0..1.10.0 has 21 commits, several in the HTTP/1
dispatcher; tokio 1.51.0..1.52.0 has 22 commits, 8 files touching io_uring. Both pairs were
therefore re-run at the fixing commit and its parent (`commit_level_check.sh`, which also
checks that the lockfile really uses the requested commit):

| Case | Parent (buggy) | Fixing commit | Result |
|---|---|---|---|
| hyper#3995 | 156a6f6 | b7a679b (#4069) | violation at the parent, clean at the fix |
| tokio#7979 | ad8c59a | c791213 (#7983) | fd leak at the parent, clean at the fix |

**pingora#931.** The #948 head differs from the 0.9.0 release only in
`pingora-memory-cache/src/read_through.rs` (the fix and its tests); the sources of
pingora-timeout, pingora-error and TinyUFO are identical (compared file by file).

**bb8 positive control** (prospective T2). The mutant differs from pristine bb8 0.9.1 only in
the 14 intended lines of `src/inner.rs` (plus the absent `.cargo_vcs_info.json` metadata).

**Differences from the reporters' programs.** "Reporter's program" above means the report's
mechanism and code shape, adapted as follows:

| Case | Adaptation |
|---|---|
| tokio#6877 | simplex capacity 5 instead of 197 and 3 rounds instead of 2,048, so that `write_all` suspends mid-pattern within a short run; `biased;` instead of random branch order; the fixed variant (both operations kept alive) is ours, following the maintainer's diagnosis |
| tokio#3825 | one client instead of two; the 2 s interval tick is replaced by Dropwise's `Preempt` with `Race::After(5s)` against the report's notification at 4 s; the fixed variant is the maintainer's code |
| redis-rs#851 | the harness also uses a multiplexed connection; waits of 50 ms instead of 1 s; fixed stream keys instead of random IDs; Redis 8 (the reporter's server version is unknown); the fixed variant (`BLOCK 100`) is ours, following the maintainer's advice to avoid `BLOCK 0` |
| hyper#3995 | the report involved reqwest/object_store and a runtime shut down mid-read; here a raw HTTP/1 response over an in-memory duplex, and Dropwise drops the dispatcher future (as runtime shutdown drops its task) |
| tokio#7979 | an ordinary temp file opened with `tokio::fs::File::open` after a warm-up open, not the reporter's PoC |
