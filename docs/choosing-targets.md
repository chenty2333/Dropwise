# Choosing what to cancel and what to observe

Dropwise explores the `Pending` boundaries of the futures you mark, and judges
each run by what your scenario observes afterwards. A clean report says nothing
about futures you did not mark or effects you did not look at. In our own
studies those two choices, not the exploration engine, decided what was found:
the T3 study cancelled sqlx 0.9.0 query futures, checked follow-ups with echo
queries and found nothing, while the same version loses the next borrower's
writes when `Pool::begin()` is cancelled during its round trip (sqlx#4393).
`prospective/RESCAN.md` measures this for all three prospective targets.

## Targets: where to cut

Ask what state **outlives the future and is not owned by a Rust value**. Drop
already returns owned values; it cannot undo a server-side transaction, a
protocol position, or a remote commit. Mark these first:

1. **One round trip that establishes session state.** `begin` / savepoints,
   pool `acquire` / checkout (including the pool's validation query),
   handshakes, authentication, `prepare`, `SET` / `USE`, subscribe, lease.
2. **Effects committed elsewhere before the reply arrives.** Writes and RPCs
   with side effects, `send` to a peer, commit, publish / ack. A dropped future
   loses the *reply*, not necessarily the *effect*.
3. **Hand-offs of things that are not plain Rust values.** Returning a
   connection to a pool, giving a stream to another task: the receiver inherits
   whatever request was in flight.
4. **Cancellation the library performs itself.** Internal timeouts
   (`connection_timeout`, `acquire_timeout`, tower `Timeout`), `try_join`
   short-circuits, `select!` loops inside the library. Your code never sees a
   dropped future, but the next caller sees its leftovers.
5. **Cleanup paths.** `close`, `shutdown`, `rollback`, and drop guards that
   only queue work for later.

Lower yield, in our runs: moving owned values through channels and pools
(`oneshot` plus bb8 owned connections, 12 plans, all clean) and read-only
queries whose result is simply discarded.

## Observations: what to check afterwards

- **Make a survivor do real work.** After the cut, let the next user of the
  same shared object (the next borrower, the peer, the next client of the same
  accept loop) perform an operation **with an effect**, and confirm that effect
  through an **independent observer**: a separate database connection, the
  peer's received messages, a server-side log. Echo or read-only follow-ups are
  not enough: they succeed inside a stale transaction.
- **Give test doubles the state that matters.** A fake connection whose
  validation only sleeps cannot show that an interrupted validation leaves a
  reply unread. In the rescan, the same bb8 `get()` target that was clean in
  the frozen study desynchronised the next borrower once the fake connection
  became an ordered request/response channel, including when bb8's own
  `connection_timeout` did the cancelling.
- **Judge against an allowed set, not against the completed run.** Calibrate
  the same scenario twice with the operation dropped before its first poll
  (`pre`) and twice with it completed (`post`). If the caller abandons the
  operation, the observation must equal `pre` or `post`; if the caller retries
  or carries on, it must equal `post`. Comparing only with `post` reports every
  clean rollback as a failure. If the two calibration runs of a side disagree,
  the verdict is unknown, not a violation.
- **State the contract when progress is legitimately partial.** `write_all`
  may leave a prefix written; an atomicity check would flag it. Use the allowed
  set only where the API promises all-or-nothing.
- **Observe late effects after an explicit completion signal**, not after a
  longer sleep. Task counts are hints: delayed cleanup looks like a leak, and a
  leaked replacement task looks like none.

## Template

`explore_observed` implements the calibration and the verdicts. Mark the target
with `ctx.target` or `ctx.race` as usual; the scenario returns the observation
instead of taking a `Mode` argument.

```rust
use dropwise::{explore_observed, Ctx, Expect};

async fn scenario(ctx: Ctx) -> Result<Observation, String> {
    let shared = setup().await?;
    // The target: a call from the checklist above.
    if let Some(result) = ctx.target(shared.begin()).await {
        result?.rollback().await?;
    }
    // The survivor: real work on the same shared object, confirmed by an
    // independent observer.
    shared.next_user_writes().await?;
    independent_observer_reads(&shared).await
}

let report = explore_observed(&config, Expect::Atomic, scenario);
```

`Expect` is the calling context's declared contract, not an inference. Use
`Atomic` when the caller may abandon the operation (the observation must equal
"never started" or "completed"; with several cuts, each cancelled operation
independently), `Completed` when it retries or carries on (only
"completed"), and `Undone` when the result must be handed to the caller: a
message, job or token that was taken out of the shared object and then lost does
not look different from completion, so only "never started" is allowed.

Calibration re-runs the same scenario twice for each side, so scenario-local
state must be constructible per run (the rescan experiments rebuild their pool
and table each time). `pre` drops the cut targets with an internal
`after_pending == 0` cut that never appears in the normal plans; if the two
samples of a side disagree the plan is `Unknown`, not a violation.

With `Config::repeat_at_site`, the plan also cancels every later target created
at the same call site. Keep the observation to the survivor's own result: if it
counts effects of the cancelled operations, a legitimate mixture (some of them
ran, some did not) equals neither calibration and is reported as a violation.
