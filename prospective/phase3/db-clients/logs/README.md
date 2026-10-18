# Raw run logs

Final 3 × full-ladder logs for the numbers in `../../RESULTS-db.md`:

- `mysql_async/release-1..3.log`: release, 10 tests (MY1/MY2 × B1/B2/B3/M/M-residue).
- `mysql_async/patched-1..3.log`: crate patch + `DW_MYSQL_GUARD=1`.
- `mysql_async/diag-b2.log`, `diag-b3.log`: `DW_DIAG=1` evidence that MY2's
  retry and different query return the cancelled query's 12345.
- `mongodb/release-1..3.log`: release on a fresh `mongo:7.0.21` replica set per repetition.
- `mongodb/patched-1..3.log`: patched mongodb (see `../patches/mongodb/`).
- `clickhouse/release-1..3.log`: release against `clickhouse/clickhouse-server:25.5.2.47-alpine`.
- `clickhouse/incompatible-24.8-*`: evidence for the 24.8.14.39 / clickhouse-arrow 0.2.1
  chunked-protocol incompatibility and the one-line patched contrast; not part of
  the cancellation ladder.

`superseded/` keeps earlier runs that are not used as numbers: the first
mongodb harness runs poisoned by blocked `dropDatabase` commands, the first
mongodb patch attempt that deadlocked M, and the standalone guarded mysql
repetitions before the crate patch was added.
