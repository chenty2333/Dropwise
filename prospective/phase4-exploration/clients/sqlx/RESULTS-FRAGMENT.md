# sqlx 0.8.6 — results fragment (round 4, unfrozen exploration)

Not Phase 4 data. One target, one client family, no commit, no upstream contact.

## Versions / environment

| item | value |
|---|---|
| client | `sqlx` 0.8.6, `sqlx-postgres` 0.8.6, `sqlx-core` 0.8.6 |
| features | `default-features = false`, `runtime-tokio`, `postgres` |
| handle | direct `sqlx::postgres::PgConnection`, no pool, no reconnect |
| tokio | 1.54.0, `MultiThread { workers: 1 }` |
| server | `postgres:18-alpine`, PostgreSQL 18.4 |
| image digest | `postgres@sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa` |
| endpoint | loopback ephemeral published port (`127.0.0.1:32885` in the recorded run) |
| observer | second `PgConnection` direct to PostgreSQL; never through the proxy |
| proxy | `dropwise::net::Proxy`, `reply_delay = 50 ms`, loopback |
| scenario | `raise` target via `execute`, `Expect::Atomic`, 2 races, max_cancellations 1, max_runs 64 |
| config | `MultiThread(1)`, races `Immediate`+`AfterWake`, settle 0/0, watchdog 10 s, scenario timeout 20 s |
| run | `logs/run.Eycpui/` (`environment.txt`, `timings.txt`, all stdout/stderr, `postgres.log`) |
| command | `./run-ladder.sh` (container `dw-r4-sqlx-$$`, trap cleanup) |

Optional same-protocol contrast in the same run: `tokio-postgres` 0.7.18
direct `Client` (`src/pg_probe.rs`, `probe-pg` phase); not part of the ladder.

## Preflight (uncancelled, one handle, independent observer)

```text
initial=[("b3",3),("t1",0),("t2",0),("t3",0)]
target_rows=3
after=[("b3",3),("t1",1),("t2",1),("t3",1)]
write_rows=4 own=7 second=7 all=[("b3",7),("t1",7),("t2",7),("t3",7)]
proxy_connections=1
PREFLIGHT-OK
```

The uncancelled target changes the rows the independent observer reads
(`t1..t3: 0 -> 1`), the same handle reads the survivor's own `7`, and the proxy
keeps exactly one client connection for the whole preflight. No target error in
the baseline.

Manual calibration, two samples per side, identical per side:

- `pre` ×2: `before=[("b3",3),("t1",0),("t2",0),("t3",0)]`,
  `write=Ok(4)`, `own=Ok(7)`, `visible=all 7`, `usable=Ok(42)`.
- `post` ×2: `before=[("b3",3),("t1",1),("t2",1),("t3",1)]`, everything else
  identical.

`pre != post` only in `before`, which is the target's server-side effect. Both
sides are stable, so the calibration is valid and non-degenerate.

## Ladder (3 repetitions, identical counts)

| level | plans/rep | violations | unrealized | unsettled | baseline_errors | exhaustive |
|---|---:|---:|---:|---:|---:|---|
| B1 `UPSERT3` | 4 | 0 | 0 | 0 | 0 | true |
| B2 `UPSERT3` | 4 | 0 | 0 | 0 | 0 | true |
| B3 `UPDATE_B3` | 4 | 0 | 0 | 0 | 0 | true |
| M1 `UPSERT3` | 4 | 0 | 0 | 0 | 0 | true |

Per repetition: 4 cut plans per level + 1 uncancelled baseline, 4 levels =
16 cut-trial executions and 4 baselines. Across 3 repetitions: **48 cut-trial
executions, 12 baselines, 0 violations, 0 unrealized, 0 unsettled,
0 baseline errors**; every report `exhaustive=true`, `baseline_targets=1`.

M1 verdicts per repetition: `pre=3 post=1 mixed=0 neither=0 unknown=0`
(3 reps: **9 Pre + 3 Post**, 12 judged cut trials). Each repetition produced
exactly one Post trial, and its plan was the same each time.

Realised plans (identical in every level and repetition; the target call site
line differs by level: B1 `src/main.rs:290`, B2 `:314`, B3 `:351`, M1 `:391`):

```text
#0@1:Immediate   #0@1:AfterWake     #0@2:Immediate   #0@2:AfterWake
```

`#0@1` and `#0@2` are the first and second realised `Pending` boundaries. They
are engine plan numbers, not protocol-stage labels. Source trace plus the Post
result identify `#0@1` as the prepare round-trip (Parse/Describe/Sync) and
`#0@2` as the bind/execute round-trip. `#0@1` and `#0@2:Immediate` were judged
`Pre`; `#0@2:AfterWake` was judged `Post` in all three repetitions.

### Concrete values (B2 / B3 / M1, all 12 cut trials per level)

```text
B2 retry after the cut:  Ok(rows_affected=3)     (expected its own 3)
B3 update after the cut: Ok(rows_affected=1)     (expected its own 1, not 3)
M1 write:                Ok(rows_affected=4)     (expected its own 4, not 3)
M1 own read:             Ok(7)
M1 visible:              [("b3",7),("t1",7),("t2",7),("t3",7)]
M1 usable:               Ok(42)
```

No cut trial returned the cancelled target's `rows_affected=3` in place of the
survivor's 4, no cut trial returned the target's server state to the survivor's
readback, and no cut trial needed a reconnect: `proxy_connections=1` in every
snapshot, and the `SELECT 42` usability probe returned 42 after every survivor.

### The executed, server-applied cut (M1 `#0@2:AfterWake`, 3/3 reps)

This is the strongest positive evidence. The target was applied on the server
before the future was dropped, but its reply had not been consumed:

```text
before  = [("b3",3),("t1",1),("t2",1),("t3",1)]   target applied server-side
after_target snapshot: residue=32 bytes            unread in the client socket
write   = Ok(4)                                    survivor's own rowcount
own     = Ok(7)
visible = [("b3",7),("t1",7),("t2",7),("t3",7)]
usable  = Ok(42)
```

The 32 bytes are unread bytes of the abandoned backend response waiting in the
client socket when the target future was dropped (the proxy delays every
server-to-client chunk by 50 ms, so framing can be split across chunks; the
probe records bytes, not a message trace). The next operation on the same
`PgConnection` did not report `Ok(3)`; it reported its own `Ok(4)`, and the
subsequent data reads were its own. In the same trials, the snapshot after the
survivor write reported `residue=0`, i.e. the leftover bytes were consumed
before/at the survivor. This is a drain, not a desynchronised shift.

`#0@1` and `#0@2:Immediate` saw `before` = the seeded 0 state, so the target had
not been applied before that observer read (immediate cuts can drop the target
before the server effect is visible). Those are `Pre`, equally allowed by
`Expect::Atomic`.

## Source mechanism

Paths below are the installed crates under
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`.

1. `PgConnection::run` calls `wait_until_ready()` **before** writing any part of
   the next query: `sqlx-postgres-0.8.6/src/connection/executor.rs:207-208`
   (and again after preparing, before Bind: `executor.rs:237-238`).
2. Every Sync increments a per-connection counter before the flush:
   `executor.rs:157-166` (`write_sync`), called for the execute phase at
   `executor.rs:276`; the non-argument simple-query path increments directly at
   `executor.rs:282-283`.
3. `wait_until_ready` flushes the write buffer, then loops `recv()` until the
   counter reaches zero, handling only `ReadyForQuery`:
   `sqlx-postgres-0.8.6/src/connection/mod.rs:85-99`; the decrement is
   `mod.rs:111-119`.
4. Therefore a dropped query future leaves the already-incremented counter at
   1. The next `execute` enters `wait_until_ready` first, **receives and
   discards the abandoned `CommandComplete` / `DataRow` messages without
   yielding them to the new query**, and only then sends its own Bind/Execute.
   The next operation is not parsing leftover backend messages as its own
   result; it parses them as drain and drops them. In the Post trial this is
   visible as `residue 32 -> 0` plus the survivor's own rowcount 4.
5. `PgStream::recv` returns an `ErrorResponse` as `Err`
   (`sqlx-postgres-0.8.6/src/connection/stream.rs:123-131`); if an abandoned
   command's error response were pending, `wait_until_ready` would surface that
   error to the next operation (fail-loudly/wrong-attribution, not silent
   data). No error-response target was exercised here.
6. Counter-hazard not exercised: `WriteAndFlush::drop` clears the outgoing
   buffer even if the write was partial
   (`sqlx-core-0.8.6/src/io/write_and_flush.rs:39-43`). If a cut landed in the
   middle of a frontend message's socket write, the rest of that message would
   be discarded while `pending_ready_for_query_count` stayed at 1; the next
   `wait_until_ready` could then wait for a `ReadyForQuery` the server will
   never send. All observed cuts were at reply reads (small messages, no
   partial write), so this is a source-level hazard and a coverage gap, not an
   observed failure.

The code comment in the pool release path is explicit that a dropped
`Executor` future/stream may leave a connection inconsistent and that the pool
should not return it unchecked:
`sqlx-core-0.8.6/src/pool/connection.rs:309-313`. The pool's response is to
`ping()` and hard-close on error; a direct `PgConnection` has no such automatic
quarantine, but this probe shows that for the exercised cuts the next
`wait_until_ready` is sufficient and the handle remains usable.

## Contrast: tokio-postgres 0.7.18 (not a ladder)

`probe-pg` waits until the independent observer sees the target applied, then
drops the still-pending `Client::execute` future while the proxy holds its
reply. 3/3 trials:

```text
before=[("b3",3),("t1",1),("t2",1),("t3",1)]   target applied
next_rows_affected=1                           its own, not the target's 3
own=7 visible=[...t1=7,t2=1,t3=1]              survivor effect + target history
client_closed=false proxy_connections=1
```

This is the same protocol and pending mechanism but a different architecture:
`tokio-postgres`'s background connection task owns the socket and continues to
drain responses even when the caller's future is gone. It is consistent with
the sqlx result (no silent stale rowcount), but it is a 3-trial probe, not a
`dropwise` ladder, and is not counted with the 48 sqlx cut trials.

## Triage / classification

- **No violation in this scenario.** B1 has no panic/timeout/obligation leak;
  B2/B3/M1 have no failed operation and no misattributed concrete value. An
  explicit error or a poisoned connection would have been a legitimate
  defensive outcome; neither occurred.
- **Classification: mature defense, not a new instance of the defect.** The
  concrete mechanism is the per-connection `pending_ready_for_query_count`
  plus `wait_until_ready` draining to `ReadyForQuery` before the next send
  (`executor.rs:207-208`, `mod.rs:85-99`). The cancelled query's rowcount and
  data are not returned to the next operation. The connection stays usable on
  the same handle and the independent observer agrees. No counterexample was
  observed in 48 cut trials: no failed next operation, no stale `Ok(3)`, no
  wrong row, no reconnect.
- **Architecture contrast:** unlike a raw framed request/response client such
  as tokio-modbus or async-pop, sqlx is running the PostgreSQL extended query
  protocol and knows how many Sync/ReadyForQuery exchanges it has opened. Its
  query path is not "read the next reply frame"; it is a state machine that
  first settles the previous exchange. That difference is exactly why the same
  "unfinished exchange" shape ends in a drain here instead of the stale
  transaction-id/response mismatch seen on the framed clients.
- **Contract status:** the crate does not publicly promise direct-handle
  recovery; the pool layer documents the opposite caution and uses a ping on
  release. The positive result is therefore an implementation-level defense,
  not a documented API guarantee. If a future version changed the counter/drain
  logic, this scenario would detect it.
- **Known vs new:** known internally is a clean sqlx 0.9.0 one-connection-pool
  response-association study (`prospective/t3-sqlx/tests/scenarios.rs`;
  `prospective/RESULTS.md` T3) and the pool comment above. Known upstream are
  adjacent reports: cancelled `begin()` on a pooled connection
  ([#4385](https://github.com/transact-rs/sqlx/issues/4385)) and
  `PgAdvisoryLock::acquire` cancel safety ([#4198](https://github.com/transact-rs/sqlx/issues/4198)).
  New here is the direct `PgConnection` 0.8.6, execution-stage cancellation
  with a server-applied three-row write, a 32-byte abandoned reply, and a
  survivor whose own rowcount (4) is distinguishable from the target's (3).
  A read-only upstream search found no exact issue for this shape; full issue
  pages could not be fetched (the host resolved to a non-public address), so
  this is "not found", not "does not exist".

## What this does not show

- One statement, one table, autocommit; no transaction, pool, TLS, COPY,
  listener, error-response target, or row streaming.
- Only two realised target Pending boundaries and two race modes; no cut in the
  middle of a partial socket write, and no cancellation of a `fetch_all` /
  `fetch_one` target. Data-return type association is inferred from the
  survivor's readback, not from a cancelled row-returning query.
- The `#0@1`/`#0@2` stage labels are inferred from source and the Post verdict;
  the engine numbers Pending boundaries, not protocol stages.
- `residue` is a socket-buffer sample, not a protocol trace; bytes already in
  sqlx's internal read buffer are not counted. `Settle { 0, 0 }` means no claim
  about effects after the run's settle window.
- No re-run against sqlx 0.9.0, no PostgreSQL version other than 18.4, no
  upstream maintainer contact, and no claim that the 0.8.6 behavior is the only
  behavior of the family.
