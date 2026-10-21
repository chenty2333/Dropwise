# skytable 0.8.12 — deep pass (round 3, unfrozen exploration)

Status: **unfrozen exploration**, 2026-10-21. This pass deepens the skytable
candidate from `RESULTS-FRAGMENT.md`; it is not Phase 4 data and makes no
preregistration claim. One client family, two reuse modes, one root-cause
family. Outcome: deterministic read-stage chain + source + experimental
fixed/original contrast; the finding stays a **已复现的复用失败候选** because
upstream is unknown and the pooled write-stage path is not byte-isolated. No
commit, no push, no upstream contact.

Raw logs: `logs/run.CncmjF/` (original + fixed ladder, deep traces) and
`logs/run.0b97xn/` (focused pooled cut). `logs/run.VlevMa/` and all earlier
`logs/run.*` are byte-untouched.

## Versions and service

- Client: `skytable 0.8.12` (crates.io exact pin); pool: official
  `skytable::pool::get_async(1, Config)`, backed by `bb8 0.9.1`; direct:
  `ConnectionAsync`.
- Server: `skytable/skytable:v0.8.4`, image id
  `sha256:823e1a390a55ddcc5579e27af58cf6f92d92a7735c5fd4fc3551673b22d08f41`
  (`skytable/skytable@sha256:823e1a39...`), disposable `dw-r3-skydeep-<pid>` /
  `dw-r3-skycut70-<pid>`, loopback-only ephemeral port, removed on exit.
- Host: Tokio 1.54.0, `Flavor::MultiThread { workers: 1 }`. Pending mechanism:
  `dropwise::net::Proxy` with `reply_delay = 50 ms` (real time), same as the
  earlier scenario; the wire trace uses an equivalent logging forwarder with
  the same 50 ms per-chunk delay.
- Config unchanged: `races=[Immediate,AfterWake]`, `max_cancellations=1`,
  `max_runs=64`, `scenario_timeout=20 s`,
  `Settle{tokio_time:0, real_time:0, watchdog:10 s}`.
- `RUST_LOG=debug` was set for deep/wire phases. skytable, bb8 and dropwise
  emit no `log`/`tracing` records and there is no request-level server log
  (the container log has startup lines only), so byte/timing instrumentation is
  used instead of `RUST_LOG` output. This is recorded in `environment.txt`.

## 1. Contract: silent on cancellation/reuse

Public pages searched (raw text saved under `logs/run.CncmjF/contract/`):

- <https://docs.rs/skytable/0.8.12/skytable/aio/struct.TcpConnection.html> —
  `query`: “Run a query and return a raw `Response`”; `execute_pipeline`:
  “Execute a pipeline. The server returns the queries in the order they were
  sent (unless otherwise set).”; `reset_buffer`: “Call this if the internally
  allocated buffer is growing too large and impacting your performance.
  However, normally you will not need to call this”.
- <https://docs.rs/skytable/0.8.12/skytable/pool/index.html> — “This module
  provides ways to create connection pools. For async connections we use
  [`bb8`] while for sync connections we used [`r2d2`].” `get_async`:
  “Returns an async TCP (skyhash/TCP) connection pool using [`bb8`]'s default
  settings and the given maximum pool size”.
- <https://docs.rs/skytable/0.8.12/skytable/aio/struct.ConnectionAsync.html> —
  same methods, no additional semantics.
- <https://docs.skytable.io/protocol/specification> — “Query-time: This works
  like a request/response action where the client sends a request and the
  server responds with a response to the query (or an error)”; pipeline
  responses: “the server will return the responses in the same order”. The
  simple response is `0x10<u16 error>`, `0x11<column cnt>...`, `0x12` (Empty) or
  `0x13<row count>...`; there is **no request id, transaction id, or
  correlation field** in the public protocol.
- <https://docs.rs/bb8/0.9.1/bb8/struct.Builder.html#method.test_on_check_out>
  (pool library, not skytable): “If true, the health of a connection will be
  verified through a call to `ManageConnection::is_valid` before it is provided
  to a pool user. Defaults to true.”

Keyword scan (`contract/scan.txt`): the skytable pages contain no
`cancel`, `drop`, `timeout`, `reuse`, `reconnect` or `abort`; the only `timeout`
hits are bb8 pool-builder settings (pool config, not query cancellation), and
`reset` occurs only as `reset_buffer`, which shrinks an internal byte buffer,
not a connection reset. The skytable crate exposes **no** close/reconnect/reset/cancel API for a
connection (grep of `pub fn` in the copied 0.8.12 source; same in 0.8.12
`sync.rs`).

**Conclusion: silent contract.** There is no documented guarantee about
dropping a query future, no documented pool validation behaviour, and no
documented recovery action. This is not `contract-external`: no page says
“not guaranteed” or “discard the connection”; it simply says nothing. The
pool-validation query is an implementation detail: `ConnectionMgrTcp::is_valid`
sends `sysctl report status` and `has_broken` is hard-coded `false`
(`skytable-0.8.12/src/pool.rs:117-122`).

## 2. Upstream: no matching report found (upstream unknown)

Read-only only; no issue/PR/email/Discord contact. Details:
`logs/run.CncmjF/contract/upstream-search.md`.

- `skytable/client-rust`: all 34 issues/PRs enumerated; 0 mention
  cancel/drop/desync/stale-reply/pool-reuse. #11 implements the pool; #13 is a
  pooling feature request; #17 is about `&mut self`; #27 fixes decoder/response
  issues. HTML issue searches for `cancel`, `drop`, `desync`, `stale reply`,
  `pool reuse`: 0 links each.
- `skytable/skytable` (server): `cancel` -> #324 “Implement NodeJS client
  driver” (unrelated); `drop` -> 11 DDL/termination/Windows-flush issues
  (unrelated); `desync`, `stale reply`, `pool reuse` -> 0.
- Upstream state: `client-rust` HEAD `4fceb22` (2025-02-16) == tag `v0.8.12`;
  no later commit. crates.io newest is still `0.8.12` (updated 2025-02-16).

**Known vs new: not found upstream.** Call this **upstream unknown**, not “new”:
not finding a report does not prove novelty, and we did not contact upstream.

## 3. Source trace

Original crate (registry copy used to build the fixed copy):
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/skytable-0.8.12/`.

- `src/io/aio.rs:126-129` — `TcpConnection { con, buf }`. `buf` is per
  connection and persists across calls; response decoder state (`state`,
  `cursor`, `expected`) is **local to the `query` future**.
- `src/io/aio.rs:183-212` — `query()`:
  `buf.clear()`; `q.write_packet(&mut buf)`; `con.write_all(&buf).await?`
  (`:184-186`); read loop `con.read(&mut chunk).await` (`:191-193`); append to
  `buf` and decode (`:200-208`); `return Ok(resp)` on `Completed` (`:203`).
  There is no drop guard, no “unread reply” flag, no stream shutdown on cancel.
  On the next call, `buf.clear()` (`:184`) resets the decode buffer but not the
  TCP stream: the pending reply is read as if it were the new query's reply.
- `src/io/aio.rs:138-147` — handshake; `src/io/aio.rs:219` — `reset_buffer`
  only.
- `src/protocol/mod.rs:90-131` — `Decoder::validate_response`; response type
  byte 0x12 is Empty; no id field anywhere. Because association is strictly
  positional, a stale `0x12` is byte-identical to the reply of
  `sysctl report status`, which is also `0x12`. That is why validation cannot
  notice the desync by inspecting the reply.
- `src/pool.rs:111-123` — bb8 `ManageConnection for ConnectionMgrTcp`:
  `is_valid` sends `sysctl report status`;
  `has_broken(&self, _: &mut ConnectionAsync) -> false`.
- bb8 0.9.1 `src/inner.rs:83-130` — `get()`: wraps the connection in
  `PooledConnection` **before** any await; the source comment at `:93-94`
  says “Cancellation safety: make sure to wrap the connection in a
  `PooledConnection` before allowing the code to hit an `await`, so we don't
  lose the connection.” Then, if `test_on_check_out`, it calls `is_valid`
  (`:110`); on `Err` it records `ClosedInvalid`, sets
  `ConnectionState::Invalid`, and loops. `put_back` at `:146-163` calls
  `has_broken` and drops the connection only if broken/expired.
  `src/api.rs:531-541` — `Drop for PooledConnection` calls `put_back`. So a
  cancelled `pool.get()` returns the connection to the pool, not to the OS.

So the actual read loop that can consume a stale reply is the same
`TcpConnection::query` read loop in both modes. In pooled mode it is reached
through `ConnectionMgrTcp::is_valid` -> `query_parse::<()>` -> `query` during
the next checkout, before user code runs.

## 4. Trigger chain, separated

### 4.1 Direct: one update future, reply association shifts

Deep wire run (`logs/run.CncmjF/deep-wire-direct-cut10.stdout.log`), times in ms
from process start, logging proxy 50 ms per server→client chunk:

```text
311.460  c2s  target update, 63 bytes, full packet  (write_all completed)
311.865  s2c  server reply 0x12 (target Empty)        (proxy reads it)
322.989  CUT  target future dropped; residue=0        (reply is in proxy, not client socket)
323.105  c2s  survivor update, 63 bytes, full packet
363.534  s2c  proxy writes the target 0x12 to the client
363.623  survivor update returns Ok(())               (it read the TARGET reply)
363.595  s2c  server reply 0x12 (survivor update)     (proxy reads it)
363.686  c2s  select
415.014  s2c  proxy writes the survivor-update 0x12; select returns
              Err("row not found (empty response)")   (it read the UPDATE reply)
415.048  s2c  server row reply 11 31 0a 05 37 0a (one row, u64 7)
465.735  s2c  select row written; diagnostic own2 = Ok(7)
```

`before=Ok(1)`, `write=Ok(())`, `own=Err("row not found (empty response)")`,
`own2=Ok(7)`, `visible=Ok(7)`. The server row was 7 the whole time; only the
client's read association was shifted by one reply. `Proxy::residue` was 0 at
the cut: the target reply had been read by the proxy but not yet by the client
socket. The stale byte reached the client at `t=363.534` and was consumed by
the survivor's `query` read loop (`aio.rs:191-203`).

Repetition: `deep-direct-cut10-rep1..3` and `deep-direct-cut1` all show the same
shape (`own=Err(empty)`, `own2=Ok(7)`, `visible=Ok(7)`); in the ladder, some
cases have `residue=1` at the cut (reply already in the client receive buffer),
same consequence.

### 4.2 Direct: second cut point is a partial request, not a stale reply

`Response::Error(6)` is “Illegal client packet” (“The client sent a malformed
query”), query error 6,
<https://docs.skytable.io/protocol/errors/> (raw text:
`logs/run.CncmjF/contract/skytable-errors.txt`), i.e. the server received
malformed query bytes. Stale **reply** bytes in the client socket can never cause that:
the server never sees them. In this scenario the malformed bytes come from
cancellation inside `con.write_all(&buf).await` (`aio.rs:186`): a prefix of the
update packet stays on the TCP stream, and the next request is appended to it. In the ladder
this appears as `retry=Err(server error 6)` / `read=Err(server error 6)` and,
in pooled M1, as `write=Err("server error 6")` with the observer confirming the
survivor write did not apply.

This is a **second manifestation of the same missing invalidation**, not a
separate root cause: both cut points are “future dropped at an await point with
no poison/close”. It was not isolated byte-for-byte for the pooled validation
write (no pooled wire run); see gaps.

### 4.3 Pooled: checkout/validation is inside the target, and validation eats the stale reply

The scenario target is `pool.get()` + update. The pool is warm (one idle
connection), so the target's first await can be bb8's checkout validation
(`sysctl report status`) rather than the update.

- **Short cut (10 ms): cancelled inside checkout validation.**
  `deep-pooled-cut10-rep1..3`: no `DEEP-REQ update-after-checkout` line (the
  update was never sent); `before=Ok(0)`; `residue=0`; next `pool.get()`
  (`t_start=11.812`, `t_end=52.738`) returns after ~41 ms, while its own
  validation reply could only arrive ~50 ms after its send (~61.8 ms): it
  consumed the target validation's stale `0x12`. Then survivor write returns
  after reading the new validation reply; survivor select reads the update
  reply and returns `Err("row not found (empty response)")`; observer is 7.
- **Late cut (70 ms): cancelled inside the update reply wait.**
  `deep-pooled-cut70-rep6..8` (run `logs/run.0b97xn/`): update sent at
  `t≈51.9`, cut at `t≈70.5`, `before=Ok(1)`. Next checkout `t_start≈70.8`,
  `t_end≈104.1`; its own validation reply is not due until ~120 ms, so it
  consumed the stale **update** `0x12`. Survivor write then consumes the
  validation reply; survivor select consumes the update reply and returns
  empty. All three reps identical. The proxy's delay is serialized per
  server→client chunk, so the next reply could not be written until ~50 ms
  after the stale write, while checkout ended ~2 ms after it.
- **Long delay (200 ms) cut at 120 ms**: same validation case with more
  separation (`deep-pooled-cut120-d200`), `before=0`, same shift.

In both pooled sub-cases the stale reply is consumed by **bb8 checkout
validation**, not by the survivor directly; the survivor receives validation's
reply. Pool state stays `connections:1, connections_created:1,
connections_closed_broken:0, connections_closed_invalid:0` in all 72
`after_target` snapshots of the original pooled ladder in `run.CncmjF`: the
pool returned the same desynchronised TCP stream every time. (The earlier
`run.VlevMa` had a few cases where validation failed and bb8 reconnected
(`connections_closed_invalid=1`, `connections_created=2`); the new run is a
cleaner “validation sees a valid `0x12`” sample. Both are consistent: only a
validation *error* triggers reconnect; a stale Empty reply looks valid.)

### 4.4 One root cause or two?

One root-cause family: **a dropped `query` future leaves the connection's
protocol position unknown, and neither the connection nor the pool manager
invalidates it.** Two cut-point manifestations:

1. drop during read → one pending reply → association shifted by one;
2. drop during write → partial request → next request is malformed (error 6).

The pooled layer adds a **third** effect: checkout validation consumes the
pending reply and hides the desync, because the validation reply is the same
`0x12` byte as the update's success reply. Calling these “the same root cause”
is justified by the fix: one drop guard on `query` removes all three observed
shapes (pooled: 0/48; direct: explicit error, no stale bytes).

## 5. Minimal experimental fix (dependency copy)

Layer: the **client connection state in the skytable crate**, not Dropwise core
and not the server. No Dropwise file is modified by the patch.

- `fix/skytable-0.8.12/` — copy of the crate with:
  - `TcpConnection` gains `poisoned: bool`;
  - a `PoisonOnDrop` guard arms at the start of `query()` and is disarmed only
    when a `Response` is decoded completely. Dropping the future at any await
    (including `write_all` or `read`) sets `poisoned = true`;
  - `query()` checks `poisoned` first and returns
    `IoError(ConnectionAborted, "skytable: query future dropped before its
    reply; connection poisoned")` without writing;
  - `is_poisoned()` is public;
  - `ConnectionMgrTcp::is_valid` rejects a poisoned connection, and
    `has_broken` now returns `conn.is_poisoned()` (original was hard-coded
    `false`). bb8 therefore discards the slot when the cancelled target's
    `PooledConnection` is dropped, and reconnects on the next `get()`.
- `fix/p4e-skytable-fixed/` — the same ladder source built against the copy via
  `[patch.crates-io] skytable = { path = "../skytable-0.8.12" }`; original
  `p4e-skytable` package still uses crates.io.
- `fix/drop-poison.patch` — unified diff, now including the pipeline guard and no-service tests.

Why this layer: the lost state is in `TcpConnection` (`buf` + the stream
position); bb8 is the reuse decision point and already has `has_broken` /
`is_valid` hooks, but the skytable manager never populated them. The patch
does not attempt protocol resynchronisation or request replay; it fails
closed and lets the pool discard.

The recorded live contrast used the original query-only patch. The reviewed
patch now also guards `execute_pipeline()` and has no-service tests for query/
pipeline cancellation, rejected reuse, completed replies and server errors.
No live pipeline contrast has been run. The sync `Connection` path has no
async drop; a poisoned **direct** connection remains open until the handle is
dropped (the next call fails, but the socket is not synchronously shut down);
there is no dedicated error variant and no auto-reconnect (auto-reconnect could
silently replay writes). Experimental, not upstream-ready.

## 6. Original vs fixed (same runner, config, server, 3 reps)

`run.CncmjF`; each cell is `violating plans / plans` (B1/B2/B3/M1 are the four
ladder levels; the original/fixed ladders are identical source except the
skytable dependency).

| mode | version | B1 | B2 | B3 | M1 | total |
|---|---|---|---:|---:|---:|---:|---:|
| pooled | original 0.8.12 | 0/4, 0/4, 0/4 | 1/4, 2/4, 0/4 | 4/4, 4/4, 4/4 | 4/4, 4/4, 4/4 | **27/48** |
| pooled | fixed | 0/4, 0/4, 0/4 | 0/4, 0/4, 0/4 | 0/4, 0/4, 0/4 | 0/4, 0/4, 0/4 | **0/48** |
| direct | original 0.8.12 | 0/2, 0/2, 0/2 | 1/2, 1/2, 1/2 | 2/2, 2/2, 2/2 | 2/2, 2/2, 2/2 | **15/24** |
| direct | fixed | 0/2, 0/2, 0/2 | 2/2, 2/2, 2/2* | 2/2, 2/2, 2/2* | 2/2, 2/2, 2/2* | **18/24*** |

`*` All fixed-direct “violations” are the explicit
`IoError(ConnectionAborted, "...connection poisoned")` returned by the next
call on the same handle; there is no stale reply, no wrong `Ok`, and no
survivor write. This is fail-closed, not a fixed level pass. The direct handle
has no reconnect/reset API in 0.8.12, so this is the smallest safe candidate
without adding API; use a fresh connection to recover.

Original run total: **42 violating executions / 72 plans** (previous
`run.VlevMa`: 35/72; same family, timing variance). One root-cause family, not
42 bugs.

### Concrete original counterexamples (`run.CncmjF`)

Direct, M1 rep1 `#0@1:Immediate`:

```text
before=Ok(1) write=Ok(()) own=Err("row not found (empty response)") visible=Ok(7)
```

The row is 7 (observer); `own` read the stale target `0x12`. Deep diagnostic
`own2` then reads the survivor's reply and returns `Ok(7)`.

Pooled, M1 rep1 `#0@2:AfterWake`:

```text
before=Ok(1) write=Err("server error 6") own=Ok(1) visible=Ok(1)
```

Target applied, survivor write got error 6 (malformed client packet from a
truncated request), own returns the old value 1; observer confirms the
survivor write did not apply.

Pooled, M1 rep2 `#0@1:AfterWake`:

```text
before=Ok(0) write=Err("server error 6") own=Ok(0) visible=Ok(0)
```

Same write-stage shape on a target that did not apply.

Pooled, M1 rep1 `#0@1:Immediate`:

```text
before=Ok(0) write=Ok(()) own=Err("row not found (empty response)") visible=Ok(7)
```

Read-stage shift: survivor write applied, own read the previous reply; the
observer sees 7.

### Fixed pooled evidence

- M1: `pre=2 post=2 mixed=0 neither=0 unknown=0` in each rep; all four plans
  match a calibration (`before=0` or `1`, `write=Ok(())`, `own=Ok(7)`,
  `visible=Ok(7)`).
- 48/72 `after_target` snapshots:
  `connections:0, connections_created:1, connections_closed_broken:1` (the
  cancelled query's `PooledConnection` was returned and `has_broken` discarded
  it). The other 24 snapshots had no query in flight yet (`connections:1`).
  After the survivor, snapshots that reconnected show `connections_created:2,
  connections_closed_broken:1` and `proxy_connections=2` (12 occurrences): a
  fresh connection was used. For comparison, original pooled `after_target`: 72/72
  `connections:1, created:1, broken:0, invalid:0`.
- Normal/reuse path (`fixed-preflight-pooled`): uncancelled
  `after_target_observer=1 survivor_write=Ok(()) own=Ok(7) visible=Ok(7)`;
  three sequential reuse writes `value=2/3/4`, `created:1`,
  `proxy_connections=1` — no reconnect, no regression.
- Fixed direct fail-closed examples: `before=0/1`,
  `write=Err(ConnectionAborted ... poisoned)`,
  `own=Err(ConnectionAborted ... poisoned)`, `visible=0/1` (target state only,
  survivor never ran). `B2/B3/M1` therefore report errors, but not stale
  values. `fixed-recovery-direct`: fresh connection
  `fresh_read=Ok(1) fresh_write=Ok(()) fresh_readback=Ok(7)` — discard+reconnect
  works; this is the only recovery path exposed by 0.8.12, not same-handle
  recovery.

### Fixed direct “no regression” scope

Uncancelled operation and reuse: 0 violations (`fixed-preflight-direct`:
normal and three sequential writes). Cancellation: fail-closed. We do **not**
claim fixed direct passes B2/B3/M1; it converts a silent association error into
an explicit unavailable-connection error. That is a correctness trade-off, not
availability recovery.

## 7. Classification

**已复现的复用失败候选 — client-side response-association / reuse defect in
skytable 0.8.12 (+ bb8 0.9.1), with experimental patch contrast; contract is
silent (also a contract gap); upstream status is unknown (no matching report
found, not contacted).** This is not a claim of a “confirmed new upstream
bug”. One root cause, two modes, two cut-point manifestations. The candidate
label is kept because upstream was not contacted and the write-stage path is
not byte-isolated for pooled validation; it is not “unresolved” because the
read-stage chain, source, and fixed/original contrast are deterministic.

Why not `contract-external`: no skytable page says cancellation makes the
connection unspecified. The public protocol has no request id and no cancel, so
a client cannot resynchronise after a drop; the fail-safe action is to
invalidate, which 0.8.12 does not do and whose pool manager explicitly reports
`has_broken=false`.

Why the pooled case is not just “caller misuse”: the pool's own checkout
validation consumes the stale `0x12` as a successful `sysctl` reply and returns
the same stream to the next borrower. The next borrower has no way to know that
its reply is one request behind. This is the user-visible consequence.

## 8. What was not covered / remaining gaps

- The write-stage error-6 path is inferred from the protocol error meaning +
  source, not isolated byte-for-byte for the pooled validation write (no
  pooled logging-proxy run). Direct wire already isolates the read-stage path.
- Post-cut `Proxy::residue` calls were added to the ladder for this pass; they
  add a small amount of work between cut and survivor and may shift timing.
  Original total was 42/72 here vs 35/72 in `run.VlevMa`; both are the same
  phenomenon, and original/fixed here share identical instrumentation.
- No generated target×successor cases rerun; no pool size > 1; no TLS, no sync
  path, no pipelines, no server restart/crash, no `After(Duration)` races, one
  client version, one server image.
- Fixed patch is experimental: live pipeline untested (the reviewed patch now
  guards it with no-service tests), direct socket not closed
  synchronously, no dedicated error variant, no auto-reconnect, no
  multi-thread/concurrent borrower test.
- Loopback disposable server only; no vendor/production Skytable; no upstream
  contact.

## 9. Artifacts and cost

- `logs/run.CncmjF/` — main suite (28 phases) + `contract/` evidence.
- `logs/run.0b97xn/` — focused pooled cut at 70 ms (3 reps).
- `logs/run.VlevMa/` — prior round-3 logs, untouched.
- `fix/skytable-0.8.12/`, `fix/p4e-skytable-fixed/`, `fix/drop-poison.patch`.
- `src/bin/sky-deep.rs`, `run-deep.sh`, `run-deep-cut70.sh`; original ladder
  source only gained `SKY-RESIDUE` prints (`src/lib.rs`, no behaviour change).
- Cost: main suite 194 s wall (28 phases) + focused cut ~3 s of runs + container
  start/build (~10 s); release builds warm. `CARGO_BUILD_JOBS=6`.

## 10. Round-4 closure: TLS, pool>1 and live pipelines (2026-10-22)

This section supersedes the corresponding §8 bullets ("no pool size > 1, no
TLS, no pipelines", "live pipeline untested", "no multi-thread/concurrent
borrower test"); the earlier sections and logs are unchanged.  No remote PR,
issue, email or upstream contact was made.

### 10.1 Patch state

- `fix/skytable-0.8.12` now poisons on a dropped or interrupted exchange in
  `query()`, `query_parse()` and `execute_pipeline()`.  The guard is armed when
  the future is first polled and disarmed only after a complete reply is
  decoded (a complete server error counts as complete).  Later operations
  return `IoError(ConnectionAborted, "skytable: connection poisoned by an
  incomplete operation")` without writing.
- `is_poisoned()` is `pub(crate)`; no public API or error variant was added.
- The async **TCP and TLS** pool managers now reject a poisoned connection in
  `is_valid()` and return it from `has_broken()`, so bb8 discards the slot at
  return and reconnects on the next checkout.  The TLS manager was the
  remaining gap: it previously hard-coded `has_broken=false`.
- The cancellation contract is documented on both entry points and in the
  pool module.
- Unit tests: 17 total, 9 lifecycle tests (unpolled query/pipeline; dropped
  query/pipeline; partial write; partial read; protocol error; EOF; complete
  success and complete server error for both entry points; `query_parse` is
  included in the blocked-reuse assertion).
- `fix/drop-poison.patch` was regenerated from a clean 0.8.12 crate.  It
  applies with both `patch -p1` and `git apply`; after applying, the two
  changed files are byte-identical to the tested copy.  The patch has no
  author or sign-off (the sender must add their own).

### 10.2 Live lifecycle matrix

`run-lifecycle.sh` starts one disposable Skytable v0.8.4 container with TCP and
TLS endpoints, generates a self-signed cert in `/tmp` (private key never
logged), builds the stock and fixed probes, and runs each mode against both.
A plain TCP `dropwise::net::Proxy` forwards the TCP and TLS bytes with a 50 ms
reply delay; the probe cuts a **pipeline** after 10 ms.  The cancelled pipeline
selects row `k=0`; the next user selects `lifecycle=4242`.  The probe uses only
the public client API, so the same source builds against both clients.

| mode | stock 0.8.12 | fixed copy |
|---|---|---|
| direct | next pipeline `Stale(0)` | `Blocked` (`ConnectionAborted`) |
| pool2: max_size=2, `test_on_check_out(false)`, one connection held | next checkout `Stale(0)` | next checkout `Fresh`; concurrent held connection also `Fresh` |
| tls-direct | next pipeline `Stale(0)` | `Blocked` |
| tls-pool: max_size=1, `test_on_check_out(false)` | next checkout `Stale(0)` | next checkout `Fresh` |

The stock results are the control, not a test assertion that stock is safe.
The fixed pool `Fresh` results show the manager discarded the poisoned slot
and reconnected.  Without the TLS manager change the next TLS call would still
be blocked by the client-side poison check (safe), but the pool would keep
returning a dead slot; the `Fresh` result distinguishes the manager fix.

Run: `logs/run.7m5uyt/` (`environment.txt`, `timings.txt`, per-mode stock/fixed
logs, `container.log`).  The stock probe binaries and the fixed probe binary
are recorded by SHA-256 in `environment.txt`.

### 10.3 Upstream status and readiness

- `skytable/client-rust` latest tag remains v0.8.12 (2025-02-16, `4fceb22`);
  no newer master commit or matching issue/PR was found when checked.  This is
  "not found", not proof of absence; no upstream contact was made.
- Decision: the patch is suitable for direct upstream review after the TLS
  manager fix and docs, not just a documentation change.  It adds no public
  API, uses the pool's existing `has_broken` mechanism, and includes tests.
  The semantic change (a cancelled connection is no longer reusable) is the
  fix and must be stated in the PR.
- Review text: `fix/PR-DRAFT.md`; local patch: `fix/drop-poison.patch`.
  No remote PR was created.

### 10.4 Remaining limits

- A cancelled request may already have executed on the server; the patch does
  not replay, roll back, or assume the server did either.  Direct users must
  reconnect; the pool reconnects on the next checkout.
- The sync (`r2d2`) path is unchanged; it has no async drop point.  The direct
  socket is not synchronously closed by the guard; the next call fails.
- Loopback only; one server image (v0.8.4); self-signed TLS certificate; no
  TLS client certificates, server restart/crash, or non-loopback/hardware.
- The live cut is timing-based (10 ms cut, 50 ms reply delay); the probe checks
  that the first future was actually cancelled, but it is not a byte-level wire
  capture of every cut.
- `test_on_check_out(true)` with a poisoned slot is covered by the
  `is_valid()` poison check and the client-side block, but the live matrix uses
  `test_on_check_out(false)` to isolate `has_broken()`; both paths are safe.
- No upstream contact, maintainer confirmation, or third-party test; no
  `Reported-by`/`Tested-by` is claimed.
