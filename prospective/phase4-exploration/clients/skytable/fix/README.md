# Fail-closed connection lifecycle fix for skytable 0.8.12

Layer: **client connection state in the `skytable` crate**, not Dropwise core,
not the server.  Local upstream candidate as of 2026-10-22; no remote PR and no
upstream contact.

## What the patch does

- `TcpConnection` gains `poisoned: bool` and a `PoisonOnDrop` guard.
  `query()`, `query_parse()` and `execute_pipeline()` arm the guard when the
  future is first polled and disarm it only after a complete reply was decoded
  (a complete business/server error counts as complete).  Dropping the future
  at any await (including a partial write or partial read), a protocol error, or
  an I/O error leaves the connection poisoned.
- A poisoned connection returns
  `IoError(ConnectionAborted, "skytable: connection poisoned by an incomplete
  operation")` on the next call, without writing.  A direct user must drop the
  connection and reconnect.
- `is_poisoned()` is `pub(crate)`; the patch adds no public API.
- The **async TCP and TLS** pool managers reject a poisoned connection in
  `is_valid` and report it in `has_broken`, so bb8 discards the slot at return
  and the next checkout opens a fresh connection.  This also holds with
  `test_on_check_out(false)`, where `has_broken` is the only defence.
- Docs state the cancellation contract on both entry points and in the pool
  module.

## Evidence

Unit tests in the patched crate (17 total, 9 lifecycle tests):

- unpolled query and unpolled pipeline do not poison;
- dropped query and dropped pipeline poison and block both entry points
  (`query_parse` included) without writing;
- partial write, partial read, protocol error and EOF poison;
- complete success and complete server error do not poison, for both query and
  pipeline.

Live lifecycle test (`run-lifecycle.sh`), one disposable server v0.8.4 with TCP
and TLS endpoints, a real TLS handshake, a plain TCP proxy with 50 ms reply
delay and a 10 ms cut.  All modes use a pipeline of one row select: the
cancelled exchange reads row `k=0`, the next user selects row `lifecycle=4242`.

| mode | stock 0.8.12 | patched copy |
|---|---|---|
| direct | `Stale(0)` | `Blocked` (`ConnectionAborted`) |
| pool2 (max_size=2, test_on_check_out=false, one connection held) | `Stale(0)` | `Fresh`; the concurrent connection is `Fresh` |
| tls-direct | `Stale(0)` | `Blocked` |
| tls-pool (max_size=1, test_on_check_out=false) | `Stale(0)` | `Fresh` |

Raw logs: `../logs/run.7m5uyt/`.  The stock result is the control; the patched
result is the expected fail-closed outcome.

## Build / run

```sh
# from clients/skytable
(cd fix/p4e-skytable-fixed && cargo build --release)
# target/release/sky-ladder <pooled|direct|...>
# target/release/sky-lifecycle <direct|pool2|tls-direct|tls-pool> <fixed|original>

./run-lifecycle.sh     # starts its own server and TLS certs in /tmp
```

The unit tests run in the patched copy:

```sh
cd fix/skytable-0.8.12
cargo test --lib
```

`drop-poison.patch` is the diff against a clean 0.8.12 crate.  The copy is
already patched; to dry-run on a clean crate:

```sh
cd /path/to/clean/skytable-0.8.12
patch -p1 < .../fix/drop-poison.patch
```

## Contract and limits

- A cancelled request may already have been executed by the server.  The patch
  does not replay, roll back, or resynchronise; it makes the connection
  unusable and lets the caller or pool reconnect.  No request IDs or cancel
  protocol exist.
- Direct connection: the next call fails explicitly; the caller reconnects.
  The socket is not synchronously closed by the guard.
- Pooled connection: the slot is discarded at return and a fresh connection is
  created on the next checkout.  Availability cost is one reconnect per
  cancelled connection; there is no automatic replay of the cancelled work.
- The sync (`r2d2`) path has no async drop guard and is unchanged.
- Covered: async TCP and TLS, direct and pooled (including pool > 1 with one
  connection held and `test_on_check_out(false)`), live pipelines, server
  errors, protocol/I/O errors.
- Not covered: sync cancellation, TLS client certificates, server
  restart/crash, non-loopback/hardware, other server versions, and a
  byte-level wire capture of every cut.  The live server is the v0.8.4 image;
  the client is 0.8.12.
- Upstream: `client-rust` v0.8.12 is still the latest tag (2025-02-16) and no
  newer master commit or matching issue/PR was found when checked; no upstream
  contact had been made at preparation time. Submitted on 2026-10-22 as
  [PR #37](https://github.com/skytable/client-rust/pull/37); the preparation
  draft is `PR-DRAFT.md`.
