# sqlx 0.8.6 — direct `PgConnection` cancellation reuse (round 4)

## Family and versions

- Client: `sqlx = "=0.8.6"`, `default-features = false`, features
  `["runtime-tokio", "postgres"]`; direct `sqlx::postgres::PgConnection` (no
  pool, no manager, no reconnect). `sqlx-core` / `sqlx-postgres` 0.8.6.
- Server: `postgres:18-alpine`, PostgreSQL 18.4, loopback-only disposable
  container with an ephemeral published port. Image id/digest:
  `sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa`
  (`postgres@sha256:1b1689b20d16…`).
- Observer: a second `PgConnection` connected **straight to PostgreSQL** with
  `DW_PG_URL`; it creates and seeds the table and reads every observation.
  The tested connection is parsed from the same URL but its host/port are
  replaced with the proxy's listener and it is connected through
  `dropwise::net::Proxy` (`reply_delay = 50 ms`), so there is exactly one
  pending mechanism.
- Tokio 1.54.0; `Flavor::MultiThread { workers: 1 }`; real time, zero settle,
  watchdog 10 s, scenario timeout 20 s.
- Optional same-protocol contrast: `tokio-postgres = "=0.7.18"` direct
  `Client` with its background connection task, against the same server and
  proxy (`src/pg_probe.rs`, `PGPROBE` lines). It is not part of the ladder.

## Operations and reuse unit

Table, reset by the observer at the start of every run:

```sql
CREATE TABLE IF NOT EXISTS dw_r4_sqlx (k text PRIMARY KEY, v int NOT NULL);
DELETE FROM dw_r4_sqlx;
INSERT INTO dw_r4_sqlx (k, v)
  VALUES ('b3', 3), ('t1', 0), ('t2', 0), ('t3', 0);
```

The tested `PgConnection` is created once per scenario run in setup, warmed
with `SELECT 1`, and held **outside** every target future. `Proxy::connections()`
is 1 for the whole run; no pool, reconnect or reset occurs before the survivor.

- Target: one multi-row write with a concrete result and a visible server
  effect, executed as `sqlx::query(TARGET).execute(&mut conn)`:

  ```sql
  INSERT INTO dw_r4_sqlx (k, v)
  VALUES ('t1', 1), ('t2', 1), ('t3', 1)
  ON CONFLICT (k) DO UPDATE SET v = excluded.v
  ```

  Expected `rows_affected = 3`. `execute` was chosen over a row-returning
  target because `rows_affected` is exactly the `CommandComplete` result that
  an abandoned exchange leaves behind, and because the survivor's rowcount (4)
  is deliberately different from the target's (3): a next operation that
  consumed the cancelled command's `CommandComplete` would report `Ok(3)` while
  also affecting four rows. The target's rows are also visible to the
  independent observer. It is a single public call but several internal
  awaits: Parse/Describe/Sync, then Bind/Execute/Close/Sync, then every backend
  message up to `ReadyForQuery`.
- B2: the same statement with the same arguments, on the same handle. Expected
  `rows_affected = 3`; only `Ok` is judged, the concrete count is logged.
- B3: a different statement, `UPDATE dw_r4_sqlx SET v = v WHERE k = 'b3'`
  (`execute`), expected `rows_affected = 1`. A stale target
  `CommandComplete` would carry 3 instead of 1.
- M1 survivor: `UPDATE dw_r4_sqlx SET v = 7 WHERE k IN ('b3','t1','t2','t3')`
  (`execute`), expected `rows_affected = 4`, then the same handle reads `t1`
  back (expected 7), then a `SELECT 42` usability probe. The independent
  observer reads **all rows** before and after. The survivor writes every row
  the target touched, so a target that is applied late (after the cut but
  before the survivor) cannot make the final observer snapshot differ from the
  `pre`/`post` calibration; only the `before` field is allowed to differ.
- Baseline (uncancelled) target must return `rows_affected = 3`; a target error
  in the baseline is a level-invalidating baseline error, not a violation.

`sqlx::query()` uses persistent prepared statements by default. Each run starts
on a fresh connection, so the target's first execution also pays the
prepare round-trip. The exploration realises two cut boundaries on the target:
`#0@1` (the prepare round-trip's reply read) and `#0@2` (the bind/execute
round-trip's reply read). The engine numbers realised `Pending`s, not protocol
stages; stage attribution comes from the source trace below and from the M1
`Post` observations (target applied server-side at `#0@2:AfterWake`).

## Observation (M1)

```text
before  = observer SELECT k,v ORDER BY k after the cut, before the survivor
write   = survivor UPDATE all four rows to 7, as rows_affected (expected 4)
own     = same-handle SELECT v WHERE k='t1' (expected 7)
visible = observer SELECT k,v after the survivor (expected all 7)
usable  = same-handle SELECT 42 (expected 42)
```

`Expect::Atomic`. Calibration, two samples per side, is stable:

- `pre` (target skipped): `before=[("b3",3),("t1",0),("t2",0),("t3",0)]`;
- `post` (target completed): `before=[("b3",3),("t1",1),("t2",1),("t3",1)]`;
- `write=Ok(4)`, `own=Ok(7)`, `visible=Ok([("b3",7),("t1",7),("t2",7),("t3",7)])`,
  `usable=Ok(42)` on both sides.

`pre != post` in `before`, so the observer distinguishes "target not applied"
from "target applied and its reply abandoned". A cancelled target that is
applied before the observer read is `Post`, not a violation by itself; a
cancelled target not yet applied is `Pre`. `write`, `own`, `visible` and
`usable` must equal calibration on either side.

`before` is a snapshot immediately after the cut. If the target is applied
after that read but before the survivor, the final observer state is still the
same as `pre` because the survivor overwrites every row the target touched, so
the trial is classified `Pre`. This is acceptable for the question here
(survivor correctness under `Atomic`), but it means `Pre` does not prove the
cancelled target had no late server-side effect. The reverse race (target
applied after the survivor's update but before the final observer read) would
produce a `Neither`; it was not observed, and the window is small because the
survivor's update itself takes one proxied round-trip.

## Contract read first

- The public `PgConnection`/`Connection`/`Executor` documentation in the
  installed 0.8.6 crate does not promise that dropping a query future leaves the
  connection usable, and there is no public per-call reset/drain method. A
  direct `PgConnection` has no pool-release check:
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlx-core-0.8.6/src/pool/connection.rs:309-313`
  says that if an `Executor` future/stream is dropped during an `.await`, the
  connection "is likely to be left in an inconsistent state, in which case it
  should not be returned to the pool"; the pool then pings and hard-closes a
  broken connection on release. That is a documented caution and a pool-layer
  band-aid, not a guarantee for direct handles and not an explicit "always
  broken" statement.
- `PgConnection::run` starts with `wait_until_ready()` before writing anything
  (`sqlx-postgres-0.8.6/src/connection/executor.rs:207-208`), and every Sync
  increments `pending_ready_for_query_count`
  (`executor.rs:157-166`, called at `executor.rs:276`). The counter and
  `wait_until_ready` are `pub(crate)`, not a public contract.
- Read-only upstream search found adjacent, not identical, reports:
  [cancelled `begin()` leaves a pooled Postgres connection with an open
  transaction (#4385)](https://github.com/transact-rs/sqlx/issues/4385) and
  [`PgAdvisoryLock::acquire` is not cancel safe (#4198)](https://github.com/transact-rs/sqlx/issues/4198).
  Those are transaction/advisory-lock shapes; neither is this direct
  mid-query `execute` case. The full issue pages could not be fetched here
  (the host resolved to a non-public address); only search snippets were read.
  No upstream issue or PR for this exact query-future/direct-handle shape was
  found.

## Pending and files

- `src/main.rs`: setup, preflight, manual calibration, B1/B2/B3/M1, realised
  plan dump.
- `src/pg_probe.rs`: optional `tokio-postgres` 0.7.18 contrast probe (not a
  ladder).
- `run-ladder.sh`: disposable `postgres:18-alpine` container named
  `dw-r4-sqlx-$$`, loopback ephemeral port, TCP readiness via the published
  endpoint plus explicit-TCP `pg_isready`, cleanup trap, fresh
  `logs/run.XXXXXX/`.
- Raw logs: `logs/run.Eycpui/` (preflight, 2 pre + 2 post manual calibrations,
  3 ladder repetitions, `probe-pg`, `environment.txt`, `timings.txt`,
  `postgres.log`). Earlier exploratory run directories are retained unchanged.

## Limits

- One statement, one table, autocommit only; no transaction, pool, TLS, COPY,
  listener, or row streaming.
- Two realised target boundaries (`#0@1` prepare, `#0@2` execute) and two race
  modes; no cut during a partial socket write, no error-response target, no
  `fetch_all`/`fetch_one` primary target. Data association is checked by the
  survivor's own readback, not by a cancelled row-returning query.
- The realised-plan labels are engine `Pending` numbers, not protocol stages.
- `Proxy::residue` is a post-cut socket-buffer sample, not a complete protocol
  trace; bytes already buffered inside sqlx are not counted.
- No upstream contact; no claim beyond 0.8.6 on PostgreSQL 18.4, and no claim
  that this covers every query/result shape.
