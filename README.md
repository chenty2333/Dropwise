# Dropwise

Cancellation-correctness testing for async Rust (Tokio).

The question Dropwise asks is not "is this future cancel-safe?" but: after a
cancellation, is every responsibility already taken, every effect already
committed, and every piece of work still running handled in a way *this calling
context* allows? For example, a received message must be delivered, requeued or
explicitly abandoned; a connection may be closed after an interrupted request
but not reused as if it were at a message boundary.

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

### Reading a report

A run can come back without a violation for reasons that are not success. The
`Report` keeps them apart, and `Display` prints every one of them:

| signal | meaning |
|---|---|
| `baseline_errors` | the *uncancelled* run already fails or leaks: fix that before reading the trials |
| `violations()` | a plan found a problem: the invariant failed, or an `Obligation` was dropped unresolved or left outstanding |
| `unrealized()` | a planned `Pending` boundary was never reached, so that plan tested nothing |
| `unsettled()`, `!baseline_settled` | `Settle::watchdog` cut the observation window short: late effects may be missing, so a clean result is inconclusive |
| `!exhaustive` | `max_runs` did not cover the frontier, or the scenario marked no target at all |
| `is_clean()` | no baseline error or trial violation was *observed* — does not imply realization, settling or exhaustive exploration |

`assert_cancel_correct` requires all of them: clean, exhaustive, settled, realized.

### What is and is not guaranteed

- A plan is a **`Pending` boundary of a marked future**, not a source-level
  `.await`: one `.await` may return `Pending` many times, and awaits that finish
  immediately are never boundaries. Unmarked futures dropped inside
  combinators are not enumerated.
- `exhaustive` means every plan within the configured limits was run for this
  scenario's input and schedule. It is not coverage of all inputs, schedules or
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
cargo run --example quickstart   # the usage shape above, runnable end to end
cargo test                       # harness tests
cargo clippy --all-targets
```

Core CI runs the tests, Clippy, API documentation build and quickstart on stable
Rust. It does not run the separate research experiments or claim their results.
Formatting is not gated yet: the existing tree does not match default rustfmt.

The root crate is the library plus its tests and examples: `cargo test` needs a
Rust toolchain and nothing else — no upstream checkouts, no services, every
scenario in-process on `tokio`.

The research halves are separate crates and documents, and are **not** part of
that command:

- `repro/` — paired reproductions against upstream releases, each with its own
  `Cargo.toml` and pinned versions (one case needs a Redis in Docker). Run with
  `repro/run_all.sh`.
- `prospective/` — pre-registered experiments on third-party crates
  (`t1`-`t3`, `phase2`). Use `prospective/run.sh t1-tungstenite` or
  `prospective/run.sh t2-bb8`; T3 and Phase 2 have their own runners:
  `prospective/t3-sqlx/run.sh` and `prospective/phase2/run.sh`.
- `survey/`, `research/` — the bug survey (`survey/survey.py`, codebook) and
  the feasibility probes.

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
