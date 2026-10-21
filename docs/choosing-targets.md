# Choosing what to cancel and what to observe

Dropwise explores the `Pending` boundaries of the futures you mark, and judges
each run by what your scenario observes afterwards. A clean report says nothing
about futures you did not mark or effects you did not look at. In our own
studies those two choices, not the exploration engine, decided what was found:
the T3 study cancelled sqlx 0.9.0 query futures, checked follow-ups with echo
queries and found nothing, while the same version loses the next borrower's
writes when `Pool::begin()` is cancelled during its round trip (sqlx#4393).
[`prospective/RESCAN.md`](../prospective/RESCAN.md) measures this for all three
prospective targets.

After a cancellation, ask three separate questions:

1. **What did the cancelled operation do?** It may legally have completed, not
   started, or left a prefix/subset that its contract allows.
2. **What did the next user get?** Its own result, an old reply, an error from
   the previous exchange, or a hang?
3. **What state remains?** Connection, protocol position, transaction, queue,
   lock, metadata, task. "The next call returned `Ok`" answers none of these by
   itself.

Keep the answers in separate observation fields. The scenario-design details are
in [writing-scenarios.md](writing-scenarios.md) §2.5.

## Targets: where to cut

Ask what state **outlives the future and is not owned by a Rust value**. Drop
already returns owned values; it cannot undo a server-side transaction, a
protocol position, or a remote commit. Mark these first:

1. **One round trip that establishes session state.** `begin` / savepoints,
   pool `acquire` / checkout (including the pool's validation query),
   handshakes, authentication, `prepare`, `SET` / `USE`, `subscribe`, lease,
   IMAP `SELECT`. The round trip need not be long; it only needs to change
   state that the next request inherits.
2. **Effects committed elsewhere before the reply arrives.** Writes and RPCs
   with side effects, `send` to a peer (including multi-step bodies such as
   SMTP DATA or IMAP APPEND), commit, publish / ack. A dropped future loses the
   *reply*, not necessarily the *effect*. A cancellation can therefore leave
   "the effect happened, the caller saw an error, the next caller sees the
   wrong reply".
3. **Hand-offs and same-owner reuse.** Returning a connection to a pool, giving
   a stream to another task, and — importantly — a long-lived direct handle used
   again after the caller dropped one operation. The pool has a validation /
   recycle gate; a direct handle may guard reuse at the next library call or
   require explicit caller recovery under its contract. Capture which
   shape you are testing.
4. **Cancellation the library performs itself.** Internal timeouts
   (`connection_timeout`, `acquire_timeout`, tower `Timeout`), `try_join`
   short-circuits, `select!` loops inside the library. Your code never sees a
   dropped future, but the next caller sees its leftovers.
5. **Recovery and cleanup paths.** `close`, `shutdown`, `rollback`, `reset`,
   `reconnect`, `retry SELECT`, and drop guards that only queue work for later.
   A recovery action can restore the operation while leaving metadata or shared
   state dirty; make it a separate target or a separate contrast, not an
   invisible step in the survivor.

Lower yield, in our runs: moving owned values through channels and pools
(`oneshot` plus bb8 owned connections, 12 plans, all clean) and read-only
queries whose result is simply discarded.

## Observations: what to check afterwards

- **Make a survivor do real work on the same object.** After the cut, let the
  next user of the same shared object (the next borrower, the peer, the next
  client of the same accept loop, the same direct handle) perform an operation
  **with an effect**, and confirm that effect through an **independent
  observer**: a separate database connection, the peer's received messages, a
  server-side log. Echo or read-only follow-ups are not enough: they succeed
  inside a stale transaction.
- **Separate object identity from payload identity.** The object (`key`, row,
  mailbox, connection) decides where the conflict or stale reply lands. A
  unique payload marker (message id, tag, request number, value suffix) decides
  whether the returned value belongs to this request. Both are needed when the
  protocol can return an old reply that looks like a success.
- **Use a different object as a control, not as a substitute.** A survivor on
  another key can show pool/global contamination, but it deliberately avoids
  the lock or conflict on the cancelled object. mongodb MG1 is the warning:
  B2 retried the same `_id` and saw `WriteConflict`; M1 wrote another document
  and saw nothing. Do not conclude "the same object is clean" from a
  different-object survivor.
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
- **With `explore_fields`, declare one field per independent effect.** A field
  contract judges each name against its own pre/post calibration and does not
  sample mixed calibrations. Separate fields can express "first done, second
  not" legitimately; a fused total cannot, and is reported as a violation. A
  state field must be true on both calibration sides, not a value the operation
  changes.
- **State the contract when progress is legitimately partial.** `write_all`
  may leave a prefix written; a multi-effect operation may apply a subset; an
  atomicity check would flag both. `Expect::{Atomic, Completed, Undone}` covers
  the common atomic, retry and must-deliver contexts, not arbitrary prefixes or
  subsets. Check the allowed prefix/subset with an explicit scenario invariant; returning
  raw progress fields alone does not extend endpoint equality to partial progress.
- **Check recovery at more than one layer.** "The next operation works" does not
  mean the API metadata, protocol correlation and shared state are restored.
  IMAP's retry SELECT restores APPEND/SEARCH/FETCH but leaves `flags` and
  `permanent_flags` contaminated; neo4rs 0.8.0 lets the next write succeed
  while a same-pool read loses its own row; SQLx's cancelled `begin` left the
  transaction open on the server. Observe the layer your claim depends on.
- **Observe late effects after an explicit completion signal**, not after a
  longer sleep. Task counts are hints: delayed cleanup looks like a leak, and a
  leaked replacement task looks like none. `Settle` is an observation window,
  not proof of quiescence; report an unsettled window as inconclusive.
- **Keep dynamic identifiers.** Do not delete an id merely because it changes
  between runs. Normalize it symbolically (strip the run prefix, map it to a
  stable token) so `pre`/`post` stay comparable, while the identity relation —
  which reply belongs to which request — is preserved.

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

For a direct handle, create the handle in `setup`, let the target future borrow
it, and let the survivor use the same handle. Do not move the handle into the
dropped future, and do not silently reconnect before the survivor; if you test
an explicit recovery, make it a separate contrast (see
[writing-scenarios.md](writing-scenarios.md) §10).

`Expect` is the calling context's declared contract, not an inference. Use
`Atomic` when the caller may abandon the operation (the observation must equal
"never started" or "completed"; with several cuts, each cancelled operation
independently), `Completed` when it retries or carries on (only
"completed"), and `Undone` when the result must be handed to the caller: a
message, job or token that was taken out of the shared object and then lost does
not look different from completion, so only "never started" is allowed.

These three values are not a complete theory of partial progress. A prefix, a
subset or a recovery-equivalence contract has to be represented in the scenario
observation or checked with an invariant; do not force it into `Atomic`.

Calibration re-runs the same scenario twice for each side, so scenario-local
state must be constructible per run (the rescan experiments rebuild their pool
and table each time). `pre` drops the cut targets with an internal
`after_pending == 0` cut that never appears in the normal plans; if the two
samples of a side disagree the plan is `Unknown`, not a violation.

With `Config::repeat_at_site`, the plan also cancels every later target created
at the same call site. Keep the observation to the survivor's own result: if it
counts effects of the cancelled operations, a legitimate mixture (some of them
ran, some did not) equals neither calibration and is reported as a violation.
`Ctx::gap` + `Config::gap_choices` explore the wait before reuse; `gap_choices`
applies to single-cut plans. Neither dimension generates a successor or
normalizes ids for you; those remain scenario work.
