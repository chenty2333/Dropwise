# Experimental fail-closed fix for skytable 0.8.12

Layer: **client connection state in the `skytable` crate**, not Dropwise core,
not the server. This is an experiment for the round-3 deep pass, not an
upstream-ready patch.

## Contents

- `skytable-0.8.12/` — copy of the crates.io 0.8.12 driver with the patch
  already applied.
  - `TcpConnection` gains `poisoned: bool` and `is_poisoned()`.
  - `query()` and `execute_pipeline()` arm a `PoisonOnDrop` guard; dropping a polled future at any await
    (write or read) poisons the connection. The guard is disarmed only when a
    complete `Response` was decoded.
  - A poisoned connection returns
    `IoError(ConnectionAborted, "skytable: query future dropped before its
    reply; connection poisoned")` on the next call, without writing.
  - `ConnectionMgrTcp::is_valid` rejects poisoned connections and
    `has_broken` returns `conn.is_poisoned()` (upstream hard-codes `false`), so
    bb8 discards the slot at return and reconnects on the next `get()`.
- `p4e-skytable-fixed/` — the same ladder source built against the copy via
  `[patch.crates-io] skytable = { path = "../skytable-0.8.12" }`. The original
  `p4e-skytable` package is untouched and uses crates.io.
- `drop-poison.patch` — the diff against a clean 0.8.12 crate. Apply with:

  ```sh
  cd fix/skytable-0.8.12
  patch -p1 < ../drop-poison.patch
  ```

  (The copy is already patched; use a clean crate for a dry run.)

## Build / run

```sh
cd fix/p4e-skytable-fixed
cargo build --release
# same env/config as the original ladder; binary:
# target/release/sky-ladder <pooled|direct|...>
```

## Limits

- Pipeline now shares the guard and rejects an already-poisoned connection.
  No-service tests cover both cancellation directions, refused reuse, and
  completed replies (including server errors) without spurious poisoning; the recorded live
  contrast predates this extension and exercises query only.
- The sync connection path has no async drop guard.
- A poisoned direct connection is not synchronously closed; the next call
  fails and the caller must drop/reconnect.
- The fix fails closed instead of attempting protocol resynchronisation.
  Auto-reconnect/replay is deliberately not attempted.
- Experimental: no dedicated error variant, no TLS/pool>1/live pipeline tests.
