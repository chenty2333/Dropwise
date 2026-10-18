# Phase 3 database clients

One integration test per target, all five levels, shared reporting from
`../../support/ladder.rs`:

- `tests/mysql_async.rs` — MY1 (pool `START TRANSACTION`) and MY2 (directly
  held `Conn`, known-value `SELECT`).
- `tests/mongodb.rs` — MG1 (first in-transaction command) and MG2 (ordinary
  read) over `mongodb` 3.9.1's pool.
- `tests/clickhouse.rs` — CH1 (`USE`) and CH2 (ordinary read) over the
  clickhouse-arrow 0.2.1 bb8 manager.

Run everything from the repository root:

```sh
bash prospective/phase3/run-db.sh                 # release, 3 reps, all targets
TARGETS=mysql_async REPS=1 bash prospective/phase3/run-db.sh
VARIANT=patched bash prospective/phase3/run-db.sh  # mysql + mongodb patch contrasts
```

clickhouse-arrow has no patched variant: the ladder found no violation, so
`VARIANT=patched` repeats the release run for that target.

The runner starts loopback-only containers, passes `DW_MYSQL_URL`,
`DW_MONGO_URL` and `DW_CLICKHOUSE_ADDR`, and stops them on exit. Pinned images:
`mariadb:11.4`, `mongo:7.0.21`, `clickhouse/clickhouse-server:25.5.2.47-alpine`
(the 24.8 pairing is unusable with clickhouse-arrow 0.2.1; see
`../RESULTS-db.md` and `../AMENDMENTS.md` #7).

`patches/` holds only triage contrasts:

- `mysql_async/`: refuse a new command on a connection whose previous routine
  was dropped; the scenario pairs it with `DW_MYSQL_GUARD=1`, which checks
  `Conn::is_disconnected()` and replaces the connection.
- `mongodb/`: abort a transaction left in the `Starting` state on
  `ClientSession::drop`, and wait for that abort before a new session starts.

Raw logs and their mapping are in `logs/README.md`.
