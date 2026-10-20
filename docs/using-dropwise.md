# Using Dropwise in your own project

Dropwise is a library plus a small set of runnable examples. This page is the
short path for a Rust developer who does not know the research repository. The
[README](../README.md) has the design overview; the research notes under
`prospective/`, `repro/` and `research/` are not needed to use the crate.

## Add it as a development dependency

This checkout is **not published on crates.io yet**. Point Cargo at the checkout
or at the git repository:

```toml
[dev-dependencies]
dropwise = { path = "../Dropwise" }          # a local checkout
# or: dropwise = { git = "https://github.com/chenty2333/Dropwise" }

tokio = { version = "1", features = ["rt", "macros", "sync", "time"] }
```

The `tokio` line is only needed when your scenario uses Tokio types such as
`tokio::task::yield_now` or `tokio::sync::mpsc`; Dropwise itself brings the
runtime it needs for each run.

The crate requires Rust 1.88 (verified with `cargo +1.88.0 check --locked` and the
test suite). The default feature set has no network dependency. `net` adds an
optional loopback TCP proxy; its residue check is Linux-only. See
[Features](#features-and-platform-limits).

## Who writes what

You write all four pieces. Dropwise does not generate a scenario, infer a
contract, or know what your protocol promises.

| piece | written by | what it is |
|---|---|---|
| target operation | you | the future passed to `ctx.target` (or `ctx.race`) whose cancellation is studied |
| survivor | you | the next user of the same handle/pool/object, doing work with an effect |
| independent observer | you | a read that does not go through the cancelled object: another connection, the peer's log, a server-side record |
| contract | you | `Expect::Atomic` / `Completed` / `Undone`, plus your invariant and allowed observation |

`ctx.target` drops the marked future for you and returns `None`. The scenario
code after it is the caller. `ctx.race` is for a real `select!`: it returns the
target future and a competitor, and the program's own `select!` drops or keeps
the losing branch. Use `ctx.race` when the cancellation is performed by the
program (a loop with a heartbeat, `select!` with a shutdown signal); use
`ctx.target` for one operation whose future you are willing to drop directly.

## The shapes

Four runnable, in-process examples cover the entry points. They are teaching
models, not real clients or pools:

```sh
cargo run --example quickstart       # a cancellable worker loop; buggy + fixed
cargo run --example direct_handle    # long-lived handle outside the target future
cargo run --example pooled_handoff   # one-connection pool, handed to the next user
cargo run --example limits           # unknown calibration, no Pending, truncation
```

All four assert their expected outcome and exit successfully. They do not need a
server, Docker or network.

The direct-handle shape:

```rust
let mut session = Session::new(server.clone());   // lives outside the target future
let cancelled = ctx.target(session.get("a")).await.is_none();
if cancelled {
    session.discard_stale_reply();                 // explicit recovery
}
let value = session.get("b").await;                // survivor, same handle
session.put("derived", value.clone()).await;       // effect
Ok(observe(&server, value))                        // independent observer
```

The pooled shape is the same scenario with a lease:

```rust
let pool = Pool::new(server.clone(), clean_on_return);
ctx.target(pool.get("a")).await;                   // lease is dropped with the target
let value = pool.get("b").await;                   // next borrower, same connection
pool.put("derived", value.clone()).await;
Ok(observe(&server, value))
```

What changes is the return path: a direct caller must recover the handle itself;
a pool's guard/return path must reset the protocol position before the next
borrower can check it out. Leaving the reset out is exactly what the buggy
variants demonstrate.

## Every run must start from fresh state

Dropwise reruns the scenario once per plan, and `explore_observed` runs the same
scenario again for calibration. Shared state outside the scenario is therefore
shared across runs and can make calibration unstable.

- Construct the pool, handle, connection, keys and server state **inside** the
  scenario closure, not once before `explore`.
- Use a fresh unique key/table/tenant per run when the external service keeps
  state. `dropwise::support::unique(prefix)` returns a process-unique value for
  this; rebuild it inside the scenario.
- Clean up in the scenario, or make setup replace the state. A scenario-local
  `Drop` guard is enough for in-process models.
- Do not put a raw unique id into the observation unless you normalize it out:
  two calibration samples with different ids would disagree and every plan would
  be `Unknown`. Observe the effect, not the run's identity.
- `Settle` controls how long the runtime keeps running after the scenario
  returns. Use `Settle::tokio_time` for Tokio tasks/timers and
  `Settle::real_time` for plain threads, blocking-pool work or kernel work. A
  watchdog that cuts this short is reported, not hidden.

## The contract is declared, not inferred

`explore_observed` compares the value your scenario returns with two
calibrations of the same scenario:

- `pre`: every cut target is dropped before its first poll (it never ran);
- `post`: no target is cancelled and every operation completes.

`Expect` says what your calling context allows:

- `Expect::Atomic`: abandoning is allowed, so the observation may equal `pre` or
  `post`. Use it when the API promises all-or-nothing and the caller really can
  walk away.
- `Expect::Completed`: the caller retries or carries on, so only `post` is
  allowed.
- `Expect::Undone`: the result must be handed to the caller (a consumed message,
  dequeued job, token), so only `pre` is allowed. A value that was taken from the
  shared object and then lost does not look different from completion.

These are **your** declarations. Dropwise cannot infer that a `write_all` may
leave a prefix, that a transaction is all-or-nothing, or that a pool resets a
connection. If the observation legitimately has a partial state that none of
these describes, do not force it into a narrower `Expect`; reshape the
observation or leave the plan undecided.

A plan with several cuts is also calibrated against every skipped/completed
mixture when the contract is `Expect::Atomic`. Each calibration side is sampled
twice. If those two samples disagree,
the plan is `Unknown`; a calibration run that fails or leaks is a
`Report::calibration_errors` entry. Neither is a violation, and neither counts
as a clean result.

## Pending boundaries and real I/O

A plan cuts at an **observed `Pending` of a marked future**, not at a source
`.await`. One `.await` can return `Pending` many times, and an await that
finishes immediately is never a boundary. Unmarked futures dropped inside
combinators are not enumerated.

Under `Flavor::CurrentThread`, Tokio time is paused and timers/scheduling are
reproducible. Real I/O, the blocking pool, plain threads, the OS scheduler and
server-side ordering are outside Tokio's control, so:

- a plan is a boundary of *this* run, not a stable source location;
- a target can move, a `Pending` count can change, and a planned cut can be
  reported as `Unrealized`;
- a clean report covers only the marked futures and the effects the scenario
  observed, for the input and schedule it ran.

Use `Flavor::MultiThread` and real time when the code under test needs real I/O;
expect `Unrealized` plans rather than deterministic coverage.

## Reading a report

`Display` prints only what needs attention plus a final result line. Use
`Report::diagnostics()` for every trial's plan, call sites, outcomes, gaps,
verdict, observation/calibration comparison, invariant and leaks.

| signal | meaning |
|---|---|
| `baseline_errors` | the uncancelled run already fails or leaks; fix that before the trials |
| `violations()` | a plan observed a problem: invariant failed, or an obligation was dropped unresolved or left outstanding |
| `calibration_errors` | a calibration run failed, leaked or produced no observation; the plan could not be judged |
| `unknown()` | a calibration side was unstable (or absent): no verdict, not a violation |
| `unrealized()` | the planned `Pending` boundary was never reached; that plan tested nothing |
| `unsettled()`, `!baseline_settled` | the settle watchdog cut the observation window; late effects may be missing |
| `truncated` | `Config::max_runs` left plans unrun |
| `!exhaustive` | the search did not cover every generated plan within the configured limits |
| no plans, `baseline_targets > 0` | a target was marked but never returned `Pending`: no cancellation point was reached |
| `coverage_gaps()` | the coverage/decidability gaps above as text (no plan, calibration, unknown, unrealized, unsettled, truncation); baseline failures and violations are separate signals |
| `notes` | hints such as `pre == post`; the observation cannot tell whether the operation happened |
| `is_clean()` | no baseline error or trial violation was *observed*; not a conclusion on its own |
| `is_clean_within_coverage()` | clean **and** no coverage gaps, baseline errors or violations |

`pre == post` is a warning about the observation, not a pass: if both
calibrations are equal, the observation cannot distinguish "the operation never
started" from "it completed", so it can only detect contamination of shared
state. `Unknown` means the verdict cannot be decided; `Unrealized` means nothing
was tested by that plan. A baseline failure means the uncancelled run is already
broken and the trials are not meaningful yet.

`assert_cancel_correct` and `assert_observed` use
`is_clean_within_coverage()`. They fail on a violation, calibration failure or
unknown, an unrealized or unsettled plan, a truncated search, or a run that
generated no plan. Prefer the explicit methods when you want to classify the
result yourself.

## Replaying one plan

Every trial records the plan that produced it. This is the supported way to
investigate a failure without rerunning the whole search:

```rust
use std::future::Future;

use dropwise::{Ctx, Expect, explore_observed, replay_observed};

fn scenario(ctx: Ctx) -> impl Future<Output = Result<Observation, String>> {
    // same scenario as the exploration
}

let report = explore_observed(&config, Expect::Atomic, scenario);
if let Some(failed) = report.violations().next() {
    let replay = replay_observed(&config, Expect::Atomic, &failed.replay_plan(), scenario)
        .expect("plans produced by the exploration are replayable");
    println!("{}", replay.diagnostics());
}
```

`replay` is the same function for the plain `explore` shape. The replay keeps the
cuts, competitor timing, gap substitutions, repeated-site flag, flavor, settle
and timeouts, reruns the uncancelled baseline (so a baseline failure is not
hidden), and `replay_observed` re-runs the calibration and the declared
`Expect`. It validates the plan first:

- an empty plan, a calibration-only `after_pending == 0` cut, duplicate or
  unsorted targets, a repeated-site plan with several cuts, or more cuts than
  `Config::max_cancellations` are rejected with `ReplayError`;
- otherwise it attempts exactly that plan. It is **not a deterministic replay**:
  with real I/O the target site or `Pending` count can differ. The returned
  report says `Unrealized` when the boundary was not reached, and records the
  actual call sites and comparison; it never silently substitutes another plan.

Multi-cut plans and gap-substitution plans are each supported and tested. A
combined multi-cut + gap plan is valid for replay (and tested by constructing
one), but the search expands gap choices only for single-cut plans, so it does
not generate that combination. A repeated-site plan has one cut and can carry a
gap; repeated-site plus several cuts is rejected. The plan's gap values are used
directly, so replay does not depend on the order of `Config::gap_choices`; if the
scenario registers fewer gap sites this time, the replay returns
`ReplayError::UnappliedGap` instead of silently substituting a default.

## Features and platform limits

- **default**: no network, no external service. Core tests and examples run
  in-process.
- **`net`**: `dropwise::net::Proxy`, a loopback-only TCP forwarder with byte
  counters, an optional real-time reply delay, and `residue`/`max_residue`.
  `Proxy::residue` reads the client socket's receive buffer through
  `/proc/self/fd` and `FIONREAD`, so it is Linux-only and returns
  `Unsupported` elsewhere. Call it while the client is idle and the peer has
  stopped sending; it does not read, block, close or change the socket.
- The `net` feature is off by default because the core tests need no network.
  Use `cargo test --features net` on Linux for its tests.

## Scope, limits, and what is not claimed

Dropwise is a dynamic checker for a scenario's marked operations and observed
effects. It does not prove a client correct, enumerate every source-level
cancellation point, control all schedules, or know your protocol's contract. A
"no violation in the covered plans" result is the useful claim; a "safe" result
is not. The examples deliberately show the difference.

The repository does not declare a license yet, so this checkout must not be
published as-is. The owner needs to choose a license before a public release.
