# PR draft: fail closed when a query or pipeline future is cancelled

Status: **local draft, 2026-10-22.  Not sent, no remote PR, no upstream
contact.**  This text is for review; the sender must add their own
`Signed-off-by`.

## Summary

A query or pipeline future that is dropped after it has been polled can leave
`skytable` 0.8.12 reading the cancelled request's reply as the next request's
reply.  The same connection is reused by bb8 because
`ConnectionMgrTcp::has_broken()` and `ConnectionMgrTls::has_broken()` return
`false`.  This patch makes the connection fail closed: a cancelled,I/O-failed
or protocol-failed exchange poisons it, later operations return
`ConnectionAborted` without writing, and the async pools discard poisoned
connections instead of returning them to the next borrower.  Direct users must
reconnect.  No public API is added.

## Root cause

The Skyhash protocol has no request ID and no cancel operation.  If the client
future is dropped after the request bytes were written but before the full
reply was consumed, the socket position is unknown.  The connection cannot
resynchronise; the only safe action is to stop using it.  The stock driver
neither tracks this state nor reports it to the pool:

- `TcpConnection` has no state for an interrupted exchange.
- `ConnectionMgrTcp::has_broken()` and `ConnectionMgrTls::has_broken()` are
  hard-coded to `false`.
- The docs do not say what happens on cancellation (contract gap).

The pool's checkout validation can itself consume the stale reply as a
successful `sysctl` reply (the exact observed case is the `0x12` empty
response), so the next borrower receives a reply that belongs to the previous
request.

## Reproduction

Local harness: a disposable Skytable v0.8.4 server (TCP and TLS), a plain TCP
proxy with a 50 ms reply delay, a 10 ms cut, and a pipeline of one row select.
The cancelled pipeline selects row `k=0`; the next user selects
`lifecycle=4242`.  The probe uses only the public client API.

| mode | stock 0.8.12 | this patch |
|---|---|---|
| direct | next pipeline returns `Stale(0)` | `ConnectionAborted` |
| pool (max_size=2, `test_on_check_out(false)`, one connection held) | next checkout returns `Stale(0)` | `Fresh`; concurrent connection `Fresh` |
| TLS direct | next pipeline returns `Stale(0)` | `ConnectionAborted` |
| TLS pool (max_size=1, `test_on_check_out(false)`) | next checkout returns `Stale(0)` | `Fresh` |

Raw logs and the probe are local (`run-lifecycle.sh`,
`src/bin/sky-lifecycle.rs`, `logs/run.7m5uyt/`).  The stock result is the
control; it is not an assertion that the stock client can be made safe by the
test.

## Fix

- Add `poisoned: bool` to `TcpConnection`.
- Arm a `PoisonOnDrop` guard when `query()` or `execute_pipeline()` is first
  polled; disarm it only after a complete reply is decoded (a complete server
  error is complete).
- On the next call, return
  `IoError(ConnectionAborted, "skytable: connection poisoned by an incomplete
  operation")` without writing.
- `query_parse()` inherits the behaviour because it calls `query()`.
- `ConnectionMgrTcp` and `ConnectionMgrTls`: `is_valid()` rejects a poisoned
  connection and `has_broken()` returns `conn.is_poisoned()`.  The bb8 pool
  therefore discards the connection at return, including with
  `test_on_check_out(false)`, and reconnects on the next checkout.
- Document the contract on both entry points and in the pool module.
- Keep `is_poisoned()` crate-private; no new public API and no new error
  variant.

## Contract

- A cancelled request may have been executed by the server.  The patch does not
  replay it, roll it back, or assume the server did either.
- A direct connection is unusable after the cancellation.  The caller must
  drop it and reconnect; the next call fails explicitly rather than silently
  reading a stale reply.
- A pooled connection is discarded and replaced on the next checkout.  The
  availability cost is one reconnect per cancelled connection; there is no
  automatic replay of the cancelled work.
- The sync (`r2d2`) path is unchanged: it has no async drop point.
- A protocol error or I/O error also poisons, because the next byte boundary is
  unknown.

## Tests

- 17 unit tests in the patched crate, including:
  - unpolled query and pipeline do not poison;
  - dropped query/pipeline poison and block `query`, `query_parse` and
    `execute_pipeline` without writing;
  - partial write, partial read, protocol error and EOF poison;
  - a complete success and a complete server error do not poison, for both
    query and pipeline.
- Live lifecycle matrix above, including a real TLS handshake through the
  delay proxy and both pool managers.  The fixed TLS pool result is `Fresh`,
  which shows the TLS `has_broken()` change is active; without it the next call
  would still be blocked by the client-side poison check (safe, but the pool
  would keep returning a dead slot).
- The patch applies with `patch -p1` and `git apply` to a clean 0.8.12 crate;
  the applied files equal the tested copy.

## Compatibility

- No API removal or signature change.  `is_poisoned()` is `pub(crate)`.
- Behaviour change: a connection whose operation was cancelled now fails
  explicitly instead of being reusable.  That is the intended fix; code that
  cancelled and then reused the same direct connection must reconnect.
- Existing `ConnectionAborted` (an `std::io::ErrorKind`) is reused, so callers
  already have a standard classification.
- Pool behaviour changes from "return the possibly desynchronised connection"
  to "discard and reconnect"; no automatic replay.

## Alternatives considered

- **Docs only**: not sufficient; the stock pool demonstrably hands the stale
  stream to the next borrower.  Docs are added, but they do not fix reuse.
- **Protocol resynchronisation / request IDs**: not available in Skyhash 0.8;
  a client cannot know which reply belongs to which request after a drop.
- **Automatic reconnect/replay in the driver**: replay can duplicate a
  non-idempotent operation and is a much larger semantic change; rejected.
- **A new public error variant**: would be a public API addition without a
  demonstrated need; `ConnectionAborted` is explicit and standard.
- **Only fix the TCP manager**: leaves the TLS pool with the same reuse path;
  both managers are patched.

## Upstream status

- `skytable/client-rust` latest tag is v0.8.12 (2025-02-16, commit `4fceb22`);
  no newer master commit or matching issue/PR was found when checked.  Not
  proof of absence; no upstream contact was made.
- This is a focused, reviewable upstream patch, not just a documentation
  change: it uses the pool's existing `has_broken` mechanism, adds no public
  API, and includes tests.  The PR can be sent after the sign-off and a final
  rebase/check.

## Sign-off

The patch file has no author or `Signed-off-by`; the sender must add their own
`Signed-off-by` (the configured repository author is
`Tianyi Chen <hi@tychen.cc>`) and attest to it.  No `Reported-by` or
`Tested-by` is claimed: this work has no third-party report or tester.
