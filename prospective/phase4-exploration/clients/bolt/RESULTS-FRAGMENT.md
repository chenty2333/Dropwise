# BOLT results fragment — round 2

Round 1 has no RESULTS-FRAGMENT.md in this directory; the first-round narrative
and tables live in ../../RESULTS.md. This file is the round-2 fragment only.

## Scope and environment

- Date: 2026-10-21 (UTC 05:42; local 14:42).
- Client family: bolt-client 0.11.0, bb8-bolt 0.8.0, bb8 0.8.6.
- Server: neo4j:5.26.0-community, digest
  neo4j@sha256:5a015e53de1895e7eee1574ae0325cf8c4b89587222778108c594bdd45a474b5,
  one disposable container dw-p4e-bolt-r2-67207, published on 127.0.0.1:32786 only.
  The container was removed by the runner's EXIT trap. No other container was
  touched.
- Toolchain: rustc 1.98.0 (88d9e12ae 2026-08-18), cargo 1.98.0 (797e8a9bc 2026-08-05).
- Runner: clients/bolt/run-round2.sh. Full environment:
  clients/bolt/logs/round2-environment.txt.
- Three repetitions per variant. All tests were selected with --exact; no
  substring/name filter was used.

## Dependency confirmation and the local patch

The installed graph is exactly bb8-bolt 0.8.0 -> bb8 0.8.6 and
bb8-bolt 0.8.0 -> bolt-client 0.11.0, with bolt-proto 0.12.0. cargo tree is in
logs/round2-deps-original.txt and logs/round2-deps-fixed.txt.

| crate | round-1 lock | round-2 original | round-2 fixed |
|---|---:|---:|---:|
| bb8 | 0.8.6 | 0.8.6 (registry) | 0.8.6 (path vendor) |
| bb8-bolt | 0.8.0 | 0.8.0 | 0.8.0 |
| bolt-client | 0.11.0 | 0.11.0 | 0.11.0 |
| bolt-proto | 0.12.0 | 0.12.0 | 0.12.0 |
| tokio | 1.54.0 | 1.54.0 | 1.54.0 |
| tokio-util | 0.7.20 | 0.7.20 | 0.7.20 |
| futures-util | 0.3.34 | 0.3.34 | 0.3.34 |
| dropwise | 0.1.0 (path) | 0.1.0 (path) | 0.1.0 (path) |
| async-trait | 0.1.92 (transitive) | 0.1.92 (direct for the harness) | 0.1.92 (direct for the harness) |

Lockfile diff versus the round-1 Cargo.lock: no package version changed. The
original lock changes only the root package name (p4e-bolt ->
p4e-bolt-r2-original) and records the already-present async-trait 0.1.92 as a
root dependency. The fixed lock additionally removes source/checksum from the
bb8 0.8.6 entry because it is a path patch. There is no multi-version
compatibility layer and no unrelated upgrade.

Difference between bb8 0.8.6 and 0.9.1 Pool::get: the only functional change
inside Pool::get is that 0.9.1 adds
self.inner.statistics.record_get_started(); immediately before the
connection_timeout timeout. The get/validation body is otherwise identical.
The 8-line t2-bb8 patch was validated on 0.9.1; on 0.8.6 its hunk lands at
inner.rs:113 instead of :107 because of earlier formatting/style differences,
so it was applied manually to the vendored copy. No semantic adaptation was
needed. Exact diff: logs/round2-bb8-pool-get.diff.

The applied change in round2/fixed/vendor/bb8-0.8.6/src/inner.rs:

~~~rust
conn.state = ConnectionState::Invalid;
match self.inner.manager.is_valid(&mut conn).await {
    Ok(()) => {
        conn.state = ConnectionState::Present;
        return Ok(conn);
    }
    Err(e) => { /* record ClosedInvalid, forward error, ... */ }
}
~~~

put_back and PooledConnection::drop are identical in 0.8.6 and 0.9.1. In both,
a connection dropped with state Invalid is removed from the pool but
connections_closed_invalid is NOT incremented (only the is_valid Err path
records ClosedInvalid). This is why the checkout evidence below uses
connections_created and proxy connections, not closed_invalid.

## Commands

~~~
cd /home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/bolt
bash run-round2.sh
~~~

The runner starts the disposable Neo4j container, writes
logs/round2-environment.txt, builds both projects offline, runs each level for
repetitions 1..3, then runs the replay test on the original project. Per level:

~~~
cd round2/original        # or round2/fixed
cargo test --offline --test round2 -- --exact <level> --nocapture --test-threads=1
~~~

Levels: normal_checkout, is_valid_failure, checkout_only, pooled_b2,
pooled_b3, pooled_m. The replay command was:

~~~
cd round2/original
cargo test --offline --test round2 -- --exact replay_checkout --nocapture --test-threads=1
~~~

## Headline result: violations and realization

Cells are violations/plans. Every plan in every run was realized; every result
line reports unrealized=0, unsettled=0, exhaustive=true, baseline_errors=0,
calibration_errors=0, baseline_targets=1, baseline_settled=true.

| level | original rep1 | original rep2 | original rep3 | fixed rep1 | fixed rep2 | fixed rep3 |
|---|---:|---:|---:|---:|---:|---:|
| checkout_only | 2/2 | 2/2 | 2/2 | 0/2 | 0/2 | 0/2 |
| pooled_b2 | 2/6 | 2/6 | 1/6 | 0/6 | 0/6 | 0/6 |
| pooled_b3 | 2/6 | 2/6 | 2/6 | 0/6 | 0/6 | 0/6 |
| pooled_m | 2/6 | 2/6 | 2/6 | 0/6 | 0/6 | 0/6 |

Across the four levels that is 60 cancellation trials per variant (6 checkout
+ 18 B2 + 18 B3 + 18 M1). **Answer: the Reset/Ready failure disappears in this
sample — the original produced 23 violations, all at #0@1:Immediate or
#0@1:AfterWake; the fixed copy produced 0.** No original violation was at @2
or @3 in this sample. The fixed @2/@3 plans were also clean; that is
outside the patch's scope, not evidence that the patch protects RUN/PULL cuts.

Original error at every checkout/B2/B3 violation, and as the write field of
every M1 violation:

~~~
Err("server gave unexpected response while in Ready state.
request: Some(Reset)
response: Success(Success { metadata: {} })")
~~~

## Checkout: what the cut actually did

Fixed checkout, all six cancellation trials (two races x three reps). Pool
notation is connections/idle; c = connections_created. Proxy is the count of
accepted proxy connections after the phase. The old leg bytes are the proxy
client-side counters for the pre-cut connection.

| rep | race | after target pool | proxy | old leg bytes | after survivor write pool | proxy |
|---:|---|---|---:|---|---|---:|
| 1 | Immediate | 0/0, c=1 | 1 | 226/251 | 1/1, c=2 | 2 |
| 1 | AfterWake | 0/0, c=1 | 1 | 226/263 | 1/1, c=2 | 2 |
| 2 | Immediate | 0/0, c=1 | 1 | 226/256 | 1/1, c=2 | 2 |
| 2 | AfterWake | 0/0, c=1 | 1 | 226/263 | 1/1, c=2 | 2 |
| 3 | Immediate | 0/0, c=1 | 1 | 226/256 | 1/1, c=2 | 2 |
| 3 | AfterWake | 0/0, c=1 | 1 | 226/263 | 1/1, c=2 | 2 |

The fixed pool therefore discards the connection before it can be returned:
connections reaches 0 immediately after the cut, the old proxy leg stops at
226/251..263 (the +6 upstream bytes are the validation RESET request; in the
AfterWake runs the RESET reply is also visible), and the survivor's next get
creates connection 2 (proxy connection 2). In all six trials the survivor's
subsequent write/read was successful (checkout observed write=Ok(7),
own=Ok(7), visible=Ok(7)). connections_closed_invalid stayed 0; see the
statistics note above.

Original checkout, all six cancellation trials:

| rep | race | after target pool | proxy | after survivor write pool | proxy | after own read pool | proxy | old leg at write |
|---:|---|---|---:|---|---:|---|---:|---|
| 1 | Immediate | 1/1, c=1 | 1 | 0/0, broken=1, c=1 | 1 | 1/1, c=2, broken=1 | 2 | 327/265 |
| 1 | AfterWake | 1/1, c=1 | 1 | 0/0, broken=1, c=1 | 1 | 1/1, c=2, broken=1 | 2 | 327/265 |
| 2 | Immediate | 1/1, c=1 | 1 | 0/0, broken=1, c=1 | 1 | 1/1, c=2, broken=1 | 2 | 327/270 |
| 2 | AfterWake | 1/1, c=1 | 1 | 0/0, broken=1, c=1 | 1 | 1/1, c=2, broken=1 | 2 | 327/270 |
| 3 | Immediate | 1/1, c=1 | 1 | 0/0, broken=1, c=1 | 1 | 1/1, c=2, broken=1 | 2 | 327/270 |
| 3 | AfterWake | 1/1, c=1 | 1 | 0/0, broken=1, c=1 | 1 | 1/1, c=2, broken=1 | 2 | 327/270 |

Original returns the half-validated connection to the idle pool (1/1). The
survivor then reuses it, the stale Reset/Ready mismatch errors, and only then
does the pool notice Defunct and count it broken. The replacement is created
only after that failure, during the observer's own read. In some Immediate
snapshots the old leg was still at 220/... at the after-target snapshot because
the proxy had not yet forwarded the RESET; by the after-write snapshot it was
at 327/..., i.e. the survivor had already attempted RUN/PULL on it.

So the failure is not an unrealized cut and not a missing validation: every
checkout plan realized, and the proxy shows the validation RESET leaving on
the old leg before the fixed copy discards it. The fixed copy's difference is
that it does not put that half-finished connection back.

## B2/B3/M1 survivor behavior

Counts are over the 18 cancellation trials per level (six plans x three reps).

| level | original | fixed |
|---|---|---|
| pooled_b2 | retry: 5 Err(Reset/Ready), 13 Ok(1); all 5 Err at @1 | retry: 18 Ok(1) |
| pooled_b3 | read: 6 Err(Reset/Ready), 6 Ok(0), 6 Ok(1); all 6 Err at @1 | read: 13 Ok(0), 5 Ok(1) |
| pooled_m | per rep: 3 Pre, 1 Post, 2 Neither; Neither observed write=Err(Reset/Ready), own=Ok(0), visible=Ok(0) | per rep: 5 Pre, 1 Post, 0 Neither; every invariant Ok(()); observed write=Ok(7), own=Ok(7), visible=Ok(7) |

The original B2 rep3 was the only 1/6 B2 run; its one violation was @1
AfterWake. All other original B2 reps had both @1 violations. B3 had both @1
violations in all three reps. M1 had both @1 violations in all three reps.
The fixed B3 Ok(0)/Ok(1) split is expected: @1 cuts can drop the target write,
while @2/@3 cuts can land after the write; both are clean for the read.

Fixed B2 rep1 run1, as a concrete discard example: after target 0/0, c=1,
proxy=1, old leg 226/256; retry Ok(1); after survivor 1/1, c=2, proxy=2, old
leg 226/256 plus new leg 220/256. Fixed B3 rep1 run1: same 0/0,c=1,proxy=1
after target; read Ok(0); after survivor 1/1,c=2,proxy=2. Fixed M1 rep1 run1:
after target 0/0,c=1,proxy=1; after survivor 1/1,c=2,proxy=2; verdict Pre,
observed write=7, own=7, visible=7. These are the same discard-and-recreate
shape as checkout.

M1 @2/@3 fixed plans are not protected by this patch (the connection has
already been returned Present after checkout); they were clean in this sample,
including the one @3 AfterWake Post plan. Do not generalize that to RUN/PULL
cancellation.

## Normal checkout and is_valid-failure paths

Both variants passed all three reps of both checks. Values below are from the
logs; wait times vary slightly and are given as ranges.

| check | original 3/3 | fixed 3/3 | concrete values |
|---|---|---|---|
| normal_checkout | pass | pass | writes [0,1,2], last 9; proxy=1; created=1; get_direct=5, get_waited=1; get_wait_time 156-161 ms |
| is_valid_failure | pass | pass | injected=1; get=Ok; write=Ok(7); proxy=2; created=2; closed_invalid=1; get_waited=2; get_wait_time 312-318 ms |

is_valid_failure injects one manager error from a FlakyManager wrapper; it
exercises bb8's real is_valid-Err/discard/retry branch, but it is not a real
server-side validation failure. It shows the patch does not break the existing
Err path or the successful checkout path.

## Replay

The replay test first explored the original checkout scenario again (2 plans,
2 violations), took the first actual violation, printed its replay_plan, and
called replay_observed with that plan.

- Source plan: #0@1:Immediate, outcome Cancelled, realized true, verdict
  Neither, settled true.
- Source call site: ctx.target(env.checkout()) at shared/round2.rs:333:31.
- Source observed: before=Ok(0), write=Err(Reset/Ready), own=Ok(0),
  tag=Ok("-survivor"), visible=Ok(0); pre/post write=Ok(7).
- Replay attempt 1: realized=true; same plan #0@1:Immediate; same site
  333:31; outcome Cancelled; verdict Neither; settled true; comparison and
  invariant are the same Reset/Ready error and the same observed values.
- Only one replay attempt ran because the first was realized; the test would
  have tried up to three. Replay result line: 1 trial, 1 violation,
  unrealized=0, exhaustive=true, truncated=false.

This is a usability result: the plan is replayable with the real proxy/server
on this host. It is not a deterministic-timing guarantee; one realized replay
does not prove the race will realize every time. The full record is
logs/round2-replay-original.log.

## Direct responsibility evidence

The distinction requested in the brief is important; the three claims do not
have the same status.

### (a) The protocol requires a server state

Yes. In READY the server accepts RUN/BEGIN. In STREAMING a result "must be
fully consumed or discarded by a client before the server can re-enter the
READY state and allow any further queries to be executed." RESET jumps ahead
of the queued work, IGNOREs the messages in front of it, discards outstanding
results, and returns the server to a new-session/READY state. PULL is valid
only in STREAMING/TX_STREAMING; its detail RECORDs are followed by a summary.
Sources:
https://neo4j.com/docs/bolt/current/bolt/server-state/#server-ready
https://neo4j.com/docs/bolt/current/bolt/server-state/#server-streaming
https://neo4j.com/docs/bolt/current/bolt/message/#messages-reset
https://neo4j.com/docs/bolt/current/bolt/message/#messages-pull

This is a protocol-level obligation on the client as a whole. It does not by
itself say which layer (application caller, bolt-client, or bb8-bolt) must
perform the cleanup.

### (b) The library promises to maintain or recover the state

Only partially, and not for this failure mode.

- bolt-client 0.11.0 does track server_state and exposes it via
  Client::server_state; every message method documents the allowed states.
  https://docs.rs/bolt-client/0.11.0/bolt_client/struct.Client.html#method.server_state
- Client::reset sends RESET and loops read_message until Success or Failure,
  ignoring IGNORED:
  https://docs.rs/bolt-client/0.11.0/bolt_client/struct.Client.html#method.reset
- bb8-bolt 0.8.0 Manager::is_valid calls conn.reset() and requires Success;
  has_broken is server_state == Defunct:
  https://docs.rs/bb8-bolt/0.8.0/bb8_bolt/struct.Manager.html

But there is no documented promise that dropping a RUN/PULL future is safe, or
that reset() repairs a queue left by a dropped future. The implementation
pairs the next socket message with the front of sent_queue; reset() reads the
next message and can therefore consume a stale Success as if it were the RESET
response. That is exactly what issue #8 says:

- meiamsome, 2021-06-12: dropping a RecordStream before the summary leaves
  the client "in a weird state where it hasn't read some of the responses yet";
  the only local fix was to relinquish the Client until the stream finished.
  https://github.com/0xSiO/bolt-rs/issues/8#issuecomment-860108448
- 0xSiO, 2021-06-13: suggests "those can be discarded with a RESET".
  https://github.com/0xSiO/bolt-rs/issues/8#issuecomment-860131118
- meiamsome, 2021-06-13: corrects that; "all of the methods currently assume
  the incoming queue of messages will be in a clean slate ... In the case of
  RESET it just reads the next message, which would probably be a RECORD or the
  SUCCESS/FAILURE result of the dropped ResultStream."
  https://github.com/0xSiO/bolt-rs/issues/8#issuecomment-860132881
- 0xSiO, 2021-06-13: acknowledges the mistake; says cleanup "sounds like
  something an async Drop would help with", and that avoiding state tracking
  was a mistake now that the protocol docs specify the states:
  https://github.com/0xSiO/bolt-rs/issues/8#issuecomment-860137879

So the library has partial state tracking but no recovery guarantee for a
cancelled future. The issue is open, and the comments are design discussion,
not a published contract.

### (c) The caller is explicitly required to reset/reconnect

Not established by the public docs or examples. bolt-client 0.11.0 ships no
examples/ directory; its only example is the lib.rs doc example, a normal
RUN/PULL sequence with no cancellation or reset path. bb8-bolt 0.8.0 has only src/lib.rs, whose example
is the normal pool use. The only explicit RESET obligation is after a server
FAILURE: the pipeline docs say "The client must
clear the failure state by sending a RESET ... or ACK_FAILURE"
(https://docs.rs/bolt-client/0.11.0/bolt_client/struct.Client.html#method.pipeline).
That is a FAILURE-state rule, not a rule for dropping an in-flight RUN/PULL
future. There is no cancellation-specific sentence in the bolt-client 0.11.0
README, crate docs, Client docs, or bb8-bolt 0.8.0 docs that says "after a
cancelled operation you must call reset or reconnect".

Therefore "caller responsibility" cannot be inferred merely because reset()
exists. A caller can choose to reset/reconnect; the round-1 M-reset and
M-reset-delayed added tests were clean, but that is a small sample and reset()
itself is the routine issue #8 warns about (it reads the next message rather
than a correlated RESET response). The status is: protocol requires a
state, the library tracks it partially, and who must repair a dropped future is
a design choice that the library has not specified. That is the honest
conclusion, not a blanket caller contract.

Open design questions (unconfirmed):
- async Drop vs an owned result/stream object vs a cancellation guard in
  bolt-client (the issue comments discuss the first two; no implementation).
- whether reset() should drain to its own RESET response instead of returning
  the first Success it sees.
- whether bb8 should discard any connection whose is_valid future is dropped
  (the local patch) or whether this belongs in manager-specific tracking.
- whether a cancelled pool checkout is common in production; it is a normal
  select/timeout shape, but no incidence data was collected here.

## What this does and does not establish

- It establishes that the exact installed bb8 0.8.6 + bb8-bolt 0.8.0 +
  bolt-client 0.11.0 stack has a cancelled-checkout window, and that the
  adapted Invalid-until-validated change removes the observed
  Reset/Ready failure in 60/60 cancellation trials across the four levels.
- It establishes discard-and-recreate concretely: after the fixed cut the pool
  has 0 connections, the old proxy leg stops, and the next get creates
  connection 2; the original returns the dirty connection to idle and only
  closes it after the survivor fails.
- It establishes that normal checkout and bb8's is_valid-Err/discard branch
  still work (the error is injected), and that the original failure plan is
  replayable with real I/O.
- It does not establish a general fix for RUN/PULL cancellation, a deterministic
  replay, an upstream bb8 decision, or a documented caller contract. The patch
  is a local 0.8.6 copy only; no multi-version compatibility layer exists.

## Artifacts

- run-round2.sh — disposable container, builds, 3 reps, replay.
- round2/shared/round2.rs — shared harness; round2/{original,fixed}/ — separate
  manifests and locks; round2/fixed/vendor/bb8-0.8.6 — patched copy.
- logs/round2-environment.txt — versions, image digest, URL.
- logs/round2-deps-original.txt, logs/round2-deps-fixed.txt — cargo tree.
- logs/round2-build-original.log, logs/round2-build-fixed.log.
- logs/round2-original-rep1.log ... rep3.log; round2-fixed-rep1.log ...
  rep3.log.
- logs/round2-replay-original.log.
- logs/round2-bb8-pool-get.diff — exact 0.8.6 vs 0.9.1 Pool::get diff.
- logs/round2-upstream-issue8.txt — exact issue-comment and protocol excerpts.
- The first-round Cargo.toml, Cargo.lock, tests/bolt.rs, tests/probe.rs,
  run.sh, environment.txt, and logs/*.log were not modified; only SCENARIO.md
  received an appended round-2 section.


## 收尾复核

2026-10-21 的独立审查、runner 修正和补跑结果见 [总报告 §9](../../RESULTS.md#9-收尾复核2026-10-21)。
本片段此前的数字是第二轮历史结果，不替代最新的补丁边界和验证结论。
