# Dropwise

Cancellation-correctness testing for async Rust (Tokio).

The question Dropwise asks is not "is this future cancel-safe?" but: after a
cancellation, is every responsibility already taken, every effect already
committed, and every piece of work still running handled in a way *this calling
context* allows? For example, a received message must be delivered, requeued or
explicitly abandoned; a connection may be closed after an interrupted request
but not reused as if it were at a message boundary. The next use can happen on
the same handle: when the caller keeps a connection across operations, the
next library call is also a reuse boundary. The library may recover or reject
reuse itself; any caller-required drain, reset, reconnect or discard must be
part of the documented contract.

## Start here

This checkout is not published on crates.io. Add it as a development dependency
by path or git, then write one target operation, one survivor and one
independent observation:

```toml
[dev-dependencies]
dropwise = { path = "../Dropwise" }   # or: { git = "https://github.com/chenty2333/Dropwise" }
```

```sh
cargo run --example quickstart       # cancellable worker loop, buggy + fixed
cargo run --example direct_handle    # handle outside the target future
cargo run --example pooled_handoff   # one-connection pool handed to the next user
cargo run --example limits           # unknown calibration, no Pending, truncation
cargo test                           # library, integration and doctests
```

All examples are in-process teaching models with no external service. The
[user guide](docs/using-dropwise.md) covers the dependency, the direct-handle and
pooled shapes, per-run reset, the `Expect` contract, report signals and plan
replay. You do not need the research trees to use the crate. The crate supports
Rust 1.88 and later.

## Current research workstreams

- [Offline discovery, round 5](prospective/offline-discovery/ROUND5-RESULTS.md):
  synchronous interruption, cross-checkout state, and field-level observation
  contracts. The synchronous repair remains a bounded research prototype.
- [Online guards](research/online/RESULTS.md): runnable quarantine/recovery
  mechanisms and real-server experiments with locally implemented strategy
  baselines. The rule study uses observed state, not configured delay/loss;
  it does not yet establish general synthesis or an upstream performance win.

## Usage

`cargo run --example quickstart` is the same shape as below, runnable, with the
report printed and annotated.

Plug into the program's own `select!` with `ctx.race`; the losing branch is
dropped (or kept) by the real code, so the post-cancellation control flow is
the program's:

```rust
dropwise::assert_cancel_correct(|ctx| async move {
    let mut rx = Receiver::from(setup().await); // messages carry obligations
    let mut sink = Sink::default();
    loop {
        let (op, mut tick) = ctx.race(recv_and_forward(&mut rx, &mut sink));
        tokio::select! {
            more = op => if !more { break },
            _ = &mut tick => {} // stands for the real competing branch
        }
    }
    check(&sink) // scenario invariant
});
```

`ctx.target(fut)` is the lighter alternative: Dropwise drops the future itself
and returns `None`, and the scenario code after it stands in for the caller.

`explore_observed` is the same engine when the scenario returns a value that
should be judged. Its plan calibrates the scenario twice, by dropping the cut
targets before their first poll (`pre`) and by cancelling nothing (`post`), and
each trial is classified against both; `Expect` says which of them the calling
context allows. See [Observed outcomes](#features) below.

What you mark and what you check afterwards decide what a run can find. See
[Choosing what to cancel and what to observe](docs/choosing-targets.md) for a
target checklist and survivor-style observations.

Dropwise reruns the scenario once per *cancellation plan* (which targets lose,
after which observed `Pending`, and when the competitor becomes ready), lets
cancelled work settle, and reports a violation when

- the scenario's invariant or an `ctx.after_settle` check fails, or
- an `Obligation` was dropped unresolved, or was still unresolved after settling
  (forgotten, or held forever by some task).

Plans whose boundary was never reached are reported as *not realized* rather
than counted as passes; `assert_cancel_correct` fails on them.

### Features

- **Targets.** `ctx.target` (Dropwise drops) and `ctx.race` (program's `select!`
  drops or keeps; outcomes `Cancelled` / `Kept` / `Stalled` / `Unrealized`).
  Plans cover up to `Config::max_cancellations` targets per run, breadth-first.
- **Race timing.** `Race::Immediate`, `Race::Reschedule(n)` (competitor ready
  after yielding to Tokio `n` times; Tokio usually runs other ready tasks first,
  but does not guarantee it), `Race::After(d)` (ready after that much Tokio
  time), `Race::AfterWake` (ready once the target has been *woken* but not
  polled again: the event reached it and was not consumed — no timing parameter).
- **Settling.** After the scenario returns, the runtime keeps running for
  `Settle::tokio_time`. Paused Tokio time does not auto-advance while
  `spawn_blocking` work runs, so this also waits for in-flight blocking-pool
  work (`tokio::fs`, stdin, DNS). `Settle::real_time` waits for work outside
  Tokio's view (plain threads, kernel). Late results carrying obligations and
  late side effects are then visible.
- **Library models.** `dropwise::models::mpsc::{Receiver, UnboundedReceiver}` hand
  out messages as `Obligation`s; `models::oblige(fut, label)` does the same for
  any future. Outside a Dropwise run obligations are untracked.
- **Observed outcomes.** `explore_observed(&config, expect, scenario)` scores the
  scenario's own value against two calibrations of the same run: `pre` drops
  every cut target before its first poll (a `Cut` with `after_pending == 0`,
  which normal plans never contain), `post` cancels nothing and lets every target
  complete. A plan with several cuts is also calibrated with every mixture of
  skipped and completed targets (`Verdict::Mixed`). Each calibration is sampled
  twice; disagreement is `Verdict::Unknown`, never a violation, and
  `assert_observed` fails on it. `Expect::Atomic` allows `pre`, `post` or a
  mixture, `Expect::Completed` only `post`, and `Expect::Undone` only `pre` for a
  result that must reach the caller (a consumed message). A `ctx.race` target
  whose program keeps polling the losing future cannot be skipped for `pre`: that
  calibration times out and is reported as a calibration failure. The
  contract is declared by the scenario author, never inferred. Calibration is
  computed on demand and cached by target set and gap assignment; a failed
  calibration run is a `Report::calibration_errors` entry (not a baseline error
  and not a panic). Equal `pre`/`post` observations add a `Report::notes` entry:
  that observation can see contamination, not whether the operation happened.
- **Plan replay.** Every `Trial` records its cancellation plan; `Trial::replay_plan`
  returns the value to pass to `replay` or `replay_observed`. Replay reattempts
  exactly that plan (cuts, race timing, gap substitutions, repeated-site flag)
  and re-runs the baseline, contract, calibration, settling and coverage checks.
  Empty, calibration-only, duplicated/unsorted, repeated-site-with-several-cuts
  or over-budget plans are rejected with `ReplayError`; a gap assignment the
  scenario no longer registers is rejected too (`UnappliedGap`), not silently
  replaced by a default. Real-I/O replays are attempts, not deterministic
  replays, and report `Unrealized` when the boundary moved.
- **Repeated cancellation at one call site.** `Config::repeat_at_site` adds, for
  each single cut, a plan that also cancels every later target created at the
  same call site at its own `after_pending`-th `Pending`. `Trial::cancelled`
  records how many instances this plan actually cancelled. Observe only the
  survivor's own result: a mixture of partial effects from the cancelled
  operations is equal to neither calibration and is reported as a violation.
- **Schedulable gaps.** `Ctx::gap(default)` returns a future for the wait before
  a shared object is reused, using `Gap::{Now, Yield(n), Sleep(d)}`. The scenario
  passes what it does in production; `Config::gap_choices` substitutes each
  choice at each gap site of a **single-cut** plan (numbered in `Ctx::gap` call
  order), so the exploration covers both "the reply arrived first" and "it did
  not". The assignment is recorded in `Trial::gaps` and included in the
  calibration cache key. A fake-server task given a `Ctx` clone can call
  `ctx.gap` too, so its reply delay is part of the explored assignment as well.
  Multi-cut plans are not expanded with gap choices; `replay` does accept a
  manually supplied plan that has both (see `Plan`), and repeated-site plans
  have one cut and can carry a gap. Repeated-site plus several cuts is rejected.
- **Residual bytes (`net` feature, Linux).** `dropwise::net::Proxy` listens on
  `127.0.0.1` only, forwards to one upstream, and counts bytes in both
  directions per client connection. `Proxy::residue()` reports how many bytes
  are unread in each client socket's receive buffer by matching
  `/proc/self/fd` entries on local and peer address and asking the kernel with
  `FIONREAD`; it never reads the data, blocks, closes the fd or changes its flags. Call it only while the
  client is idle and the peer has stopped; fd reuse is a race, and server-push
  protocols (pub/sub, notifications) are not applicable. The feature is off by
  default, so core `cargo test --locked` needs no network: run
  `cargo test --features net` for its tests.

### Reading a report

A run can come back without a violation for reasons that are not success. The
`Report` keeps them apart, and `Display` prints every one of them:

| signal | meaning |
|---|---|
| `baseline_errors` | the *uncancelled* run already fails or leaks: fix that before reading the trials |
| `violations()` | a plan found a problem: the invariant failed, or an `Obligation` was dropped unresolved or left outstanding |
| `calibration_errors` | a calibration run failed, leaked or produced no observation, so the affected plan could not be judged |
| `unknown()` | a calibration side's two samples disagreed (or a side was absent), so `explore_observed` could not judge that plan; not a violation |
| `unrealized()` | a planned `Pending` boundary was never reached, so that plan tested nothing |
| `unsettled()`, `!baseline_settled` | `Settle::watchdog` cut the observation window short: late effects may be missing, so a clean result is inconclusive |
| `truncated` | `max_runs` left plans unrun (either stopped the search or skipped an unqueueable subtree) |
| `!exhaustive` | the search did not cover every generated plan within the configured limits |
| no trials with `baseline_targets > 0` | a target was marked but never returned `Pending`: no cancellation plan was generated |
| `coverage_gaps()` | the coverage/decidability gaps above, as text (baseline failures and violations are separate signals) |
| `notes` | non-degenerate hints, e.g. this plan's `pre` and `post` observations are equal, so the observation cannot tell whether the operation happened |
| `is_clean()` | no baseline error or trial violation was *observed* — does not imply realization, settling or exhaustive exploration |
| `is_clean_within_coverage()` | clean and no coverage gaps: the strongest conclusion this report supports |

`assert_cancel_correct` and `assert_observed` use
`is_clean_within_coverage()`: no violation, baseline or calibration failure,
unknown verdict, unrealized/unsettled plan, truncated search or missing
cancellation point. `diagnostics()` prints every trial's plan, sites, outcomes,
gaps, verdict and observation/calibration comparison.

### What is and is not guaranteed

- A plan is a **`Pending` boundary of a marked future**, not a source-level
  `.await`: one `.await` may return `Pending` many times, and awaits that finish
  immediately are never boundaries. Unmarked futures dropped inside
  combinators are not enumerated.
- `exhaustive` means every plan within the configured limits was run for this
  scenario's input and schedule. It can be true for a scenario whose marked
  targets never returned `Pending` (no plan exists); check `coverage_gaps()` or
  `is_clean_within_coverage()`. It is not coverage of all inputs, schedules or
  source-level cancellation points.
- `Flavor::CurrentThread` pauses Tokio time: timers and task scheduling are
  reproducible. Real I/O, `std::time`, the blocking pool and the OS scheduler
  are not controlled, so reproducibility is high but not guaranteed.
- `Flavor::MultiThread` uses real time; pending counts can differ between runs,
  which shows up as unrealized plans.
- Drops caused by runtime shutdown after settling are not counted.

See `tests/serial_console.rs` (RFD 400 bug and fix), `tests/semantics.rs`
(`select!` integration, kept futures, unrealized plans, outstanding obligations,
late completions) and `tests/explorer.rs`.

## Development

```sh
cargo run --example quickstart       # the usage shape above, end to end
cargo run --example direct_handle    # long-lived handle outside the target
cargo run --example pooled_handoff   # pool return path and next borrower
cargo run --example limits           # inconclusive results and coverage gaps
cargo test                           # library, integration and doctests
cargo test --features net            # optional proxy/residue tests (Linux)
cargo clippy --locked --all-targets -- -D warnings
```

The crate requires Rust 1.88, verified with `cargo +1.88.0 check --locked` and
the test suite. The default feature set needs no network; `net` is optional and
its residue inspection is Linux-only.

Core CI runs the tests (including the `net` feature), Clippy, the API
documentation build and the four in-process examples on stable Rust. It does not
run the separate research experiments or claim their results.
Formatting of the root crate is gated too: CI runs
`cargo +stable fmt --check --package dropwise`, which covers `src/`, `tests/` and
`examples/` only. The separate research crates under `prospective/`, `repro/`
and `research/` keep their own formatting and are not checked by Core CI.

The root crate is the library plus its tests and examples: `cargo test` needs a
Rust toolchain and nothing else — no upstream checkouts, no services, every
scenario in-process on `tokio`.

The package manifest uses an explicit `include` list (library, README, the public
docs, examples and tests); the research/prospective/repro trees, logs and
private notes cannot enter a package. Run `cargo package --list` to inspect the
result. **No license is declared yet**, so this checkout is not ready for a
public release; the owner must choose a license first. Everything else needed to
build and consume the package independently is present.

The research halves are separate crates and documents, and are **not** part of
that command. For the current synthesis of what the experiments changed, read
[FOLLOWUP.md](research/literature/FOLLOWUP.md) and
[THEORY.md](research/theory/THEORY.md); the stage results remain the evidence.

The proposed next direction,
[progress witnesses and online reuse guards](research/theory/PROGRESS-GUARDS.md),
connects offline counterexamples to narrow runtime enforcement; it is a research
proposal, not an implemented feature.

- `repro/` — paired reproductions against upstream releases, each with its own
  `Cargo.toml` and pinned versions (one case needs a Redis in Docker). Run with
  `repro/run_all.sh`.
- `prospective/` — pre-registered experiments on third-party crates
  (`t1`-`t3`, `phase2`), plus the Phase 3 target batch and the neo4rs dynamic
  validation. Use `prospective/run.sh t1-tungstenite` or
  `prospective/run.sh t2-bb8`; T3, Phase 2 and Phase 3 have their own runners:
  `prospective/t3-sqlx/run.sh`, `prospective/phase2/run.sh` and
  `prospective/phase3/run-db.sh` / `run-http.sh`.
- [Neo4j/Bolt validation](prospective/neo4rs/RESULTS.md) — the pair-scan
  suspect is confirmed dynamically: a cancelled exchange leaves a `SUCCESS`
  that `recycle` accepts as its RESET reply, and the next reader silently loses
  its own row. The fix is only on upstream `main`; this is not a new report.
- [Phase 4 first-contact exploration](prospective/phase4-exploration/RESULTS.md)
  — unfrozen, 2026-10-21; async-smtp, async-imap and bolt-client (pooled and
  direct). Results are exploratory and are not part of the formal Phase 4
  protocol.
- `survey/`, `research/` — the bug survey (`survey/survey.py`, codebook) and
  the feasibility probes.
- [Known-mechanism comparison](research/mechanisms/RESULTS.md) — independent first-Pending
  controls, matched/full Dropwise runs and settling ablations on two historical cases.
  These are retrospective mechanism results, not new bug discoveries.
- Small interruption/composition experiments: `bash prospective/run-interruption.sh`
  runs observation-limit controls, survivor transaction-state checks, owned-connection
  handoffs, commit-before-acknowledgement retries, and Pending-window comparisons,
  three times each. It reuses the existing SQLx/bb8 crates and locked dependencies,
  starts a disposable loopback-only PostgreSQL container, and stops it on exit.
  Requires Docker, the local `postgres:18-alpine` image and cached crates. Results
  go only to stdout; frozen studies and historical results are unchanged.
  - SQLx successor observations are judged against an allowed set calibrated from
    the same scenario (operation never started, operation completed; two samples
    each, `prospective/support/allowed.rs`), not hard-coded expected business
    values. The selected projection and whether "never started" is acceptable
    remain explicit application contracts, not inferred specifications. Unstable
    calibrations or incomplete observations are unknown.
  - Controls expose legal partial progress (false atomicity alarms), lossy state
    projections and equal task counts (missed identity changes), delayed cleanup
    (early leak alarms), and external events after equal T/2T snapshots. A completion
    signal only certifies the work explicitly covered by that signal.
  - The shared test-only TCP gate withholds actual PostgreSQL reply bytes. An
    independent connection certifies commit; sampling immediately before query
    destruction distinguishes witnessed committed/unacknowledged cuts from unknowns.
    Pending enumeration with `AfterWake` can reach this window when gated; absence
    of a certificate without the gate is not evidence that the window is unreachable.
  Unsafe controls are expected to expose contamination/duplicate effects; tests pass
  when the labelled outcomes are reproduced. These are not new upstream bug claims.
- [Rescan of the prospective targets](prospective/RESCAN.md) — `bash prospective/run-rescan.sh`
  reruns T1–T3 with the [target checklist](docs/choosing-targets.md), survivor
  observations and allowed-set verdicts, and runs T3 against sqlx 0.9.0 with and
  without the upstream fix for sqlx#4393. Post hoc: the rescan was designed after
  that bug was known, so it explains the frozen study's blind spots rather than
  estimating detection rates.
  `bash prospective/run-bb8-fix.sh` checks a proposed bb8 fix for connections
  returned to the pool mid-validation (frozen T2 scenarios included) and runs
  bb8-redis, bb8-postgres and bb8-memcached against real loopback servers with
  and without it.
  `bash prospective/run-scan-clients.sh` applies the same method to sqlx's MySQL
  driver (MariaDB) and lettre's pooled SMTP transport (Mailpit), with and
  without proposed fixes.

## Bug survey

`survey/survey.py` (needs a logged-in `gh`):

```sh
survey/survey.py collect --repos survey/repos.txt [--termset symptom] [--no-commits]
survey/survey.py export     # -> data/annotate.csv, keeps annotations
survey/survey.py stats      # category shares and injectability
```

Categories and fields are defined in `survey/CODEBOOK.md`.

## Related tools

- [Asupersync](https://github.com/Dicklesworthstone/asupersync): an async runtime
  with a deterministic lab mode that injects cancellation at each recorded poll
  point and checks obligation/leak oracles. Requires its own runtime.
- [futures-testing](https://github.com/conradludgate/futures-testing): property
  testing of `Future` implementations, including cancellation between iterations.
