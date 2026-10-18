# Patch contrasts

These are not part of the release numbers. They exist to show a cause and a
fix for the violations in `../../RESULTS-db.md`.

- `mysql_async/mysql_async-0.37.1-connection-closed.diff`: at the start of
  `Conn::routine`, return `ConnectionClosed` if the previous routine was
  dropped (the connection is still marked `disconnected`). The scenario also
  sets `DW_MYSQL_GUARD=1`, which replaces a `Conn` whose
  `is_disconnected()` is true. Both are needed for a clean next use.
- `mongodb/mongodb-3.9.1-cancelled-transaction-abort.diff`: treat a
  `Starting` transaction as `InProgress` in `ClientSession::drop`, register
  and wait for Drop-time aborts, mark an abort complete only after the
  `abortTransaction` command finishes, and wait in `start_session` (not in
  `ClientSession::new`, where the abort task's own session would recurse).

`run-db.sh` copies the cached crate source to `$DROPWISE_PHASE3_WORK`, applies
the diff, and builds a copy of this scenario crate with `[patch.crates-io]`.
The 24.8 ClickHouse compatibility contrast is in
`../logs/clickhouse/incompatible-24.8-chunked-fallback.patch` because it is
outside the ladder.
