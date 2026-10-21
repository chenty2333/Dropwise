# OrientDB cancellation scenario (round 3, unfrozen exploration)

Status: exploratory, not a frozen Phase 4 protocol. One target, one Pending
mechanism, four levels, 3 repetitions.

## Versions and service

| item | value |
|---|---|
| client | orientdb-client **0.6.0**, feature `tokio-runtime` |
| pool | `aio::SessionPool` over **mobc 0.7.3** |
| runtime | Tokio 1.54 (resolved), `Flavor::MultiThread { workers: 1 }` |
| server | Docker image `orientdb:3.0.23` (Veloce, build 6f15b98a4317a01a907edc0df1f165f48742c996) |
| database | unique `plocal` database per runner, created through the HTTP API |
| ports | HTTP 2480 and binary 2424, both mapped to `127.0.0.1` ephemeral ports |
| root auth | `ORIENTDB_ROOT_PASSWORD` env, basic auth on the HTTP API |

`run-round3.sh` starts the container, waits for `GET /listDatabases`, then
creates the database with `POST /database/<db>/plocal` (HTTP 200). That is the
documented HTTP create-database path for 3.0 (the plain `/database` endpoint
answers 405). The container is removed on exit.

## Public API used (read before detection)

- `OrientDB::connect(ConnectionOptions)`; `ConnectionOptions::builder()`.
- `client.sessions(db, user, pass, min_size, max_size)` → `aio::SessionPool`.
- `SessionPool::get()` → `SessionPooled` (mobc connection guard derefs to
  `OSession`); `size/idle/used/max`.
- `OSession::{query, command, script_sql}`, `Statement::run()` +
  `futures::Stream`, `OSession::close(self)`.
- Public session fields `client_id`, `session_id`.
- No cancellation, timeout or recovery guarantee is documented on these items.

`SessionPool` is created with `max_size = Some(1)`, so at most one logical
session is checked out. The inner `OrientDB` client is created from
`ConnectionOptions` with `pool_size(1)`. **Measured:** after the first request
the proxy sees **two physical TCP connections**, and this stays at two over five
sequential requests while the pooled `session_id` stays the same. The public pool
therefore bounds one logical session, **not one physical stream**; 0.6.0's
`OrientDB::connect` does not pass `ConnectionOptions::pool_size` to its internal
`Cluster` (observed behaviour; not read in the cancellation path). Recorded, not
pretended.

## Setup (per scenario run, including every calibration run)

1. Independent `OrientDB` client connects **directly** to `127.0.0.1:<binary
   port>` (no proxy) and opens an `OSession`. This is the observer.
2. Observer creates class `DwOri` and properties `k: STRING`, `v: INTEGER`,
   `marker: STRING` if missing, then inserts one record
   `{k: "r3-<pid>-<n>-primary", v: 0, marker: "seed"}`.
3. The tested `OrientDB` client is built toward a `dropwise::net::Proxy`
   (`reply_delay = 50 ms`, every server→client chunk) and a `SessionPool` with
   `max_size = Some(1)`.
4. No request goes through the tested pool in setup, so the target's first
   `Pending` is the checkout itself.

## Target

One future: `SessionPool::get()` (checkout: TCP connect / binary `Open`
request-response) followed by one complete side-effecting request on that
session:

```sql
UPDATE DwOri SET v = 1, marker = 'target' WHERE k = '<id>';
SELECT v, marker FROM DwOri WHERE k = '<id>'
```

executed with `OSession::script_sql` (mode 2). The first reply row is
`(1, "target")`. The response is drained, so the client's detached
`PagedResultSet` close does not add a second physical connection. A cancelled
call drops the session guard inside the target future, returning it to the pool.

A supplementary held-session target (`tests/session.rs`) checks out the session
in setup and keeps it; its target is only the same script (no checkout in the
future), so the survivor uses the identical session guard.

## Pending

`dropwise::net::Proxy` with `ProxyOptions { reply_delay: 50 ms }` on the binary
port. One mechanism for every level and every calibration. Calls that can hang
are wrapped with `bounded()` (2 s) and become recorded errors.

## Survivor and observer

- Survivor: the same pool (or the same held session), the **same record**,
  `UPDATE ... SET v = 7, marker = 'survivor'`; then one read-back through the
  same pool/session.
- Independent observer: the direct `OSession` from setup (second client, no
  proxy, not the tested pool), reading `(v, marker)` before and after the
  survivor.
- Observed value: `{before, write, own, visible, pool=(size,idle,used,max)}`
  where the reply tuple carries both the value and the request marker, so a
  reply from the cancelled target can be told from the survivor's own reply.
- B1 = cancel+stop; B2 = same request, same args (only `Ok` judged); B3 = read
  on the same record through the same pool/session (only `Ok` judged);
  M1 = `explore_observed(Expect::Atomic)` with the values above.

Contract written before the run: abandoning the future is allowed; a completed
target may legitimately have written `v = 1` (`Expect::Atomic` accepts `pre` or
`post`). The survivor must receive its own `(7, "survivor")` and the observer
must see the same record value after the survivor. Nothing in the public docs
promises what happens to a cancelled checkout or query.

## Config

`Flavor::MultiThread { workers: 1 }`, `races = [Immediate, AfterWake]`,
`max_cancellations = 1`, `max_runs = 64`, `scenario_timeout = 20 s`,
`Settle { tokio_time: ZERO, real_time: ZERO, watchdog: 10 s }`.

## Generated cases

`tests/generate.rs` (separate binary, `GEN`/`CASE` prefix) uses
`prospective/support/combine.rs` with targets `update(primary, 1)` and keyless
`checkout`, successors `update(record, value)`, `update+read(record, value)`,
`read(record)`. Same/different object and same/different payload are expanded
where both sides declare the domain (13 cases). The keyless checkout target
declares object/payload `n/a` rather than inventing keys.

## Recovery contrast

The only documented close action is `OSession::close(self)`; for a pooled
session it is a no-op, and the pool has no documented cancel/reconnect method.
`tests/orientdb.rs::recovery_contrast` therefore compares the stale pool with a
**fresh `OrientDB` client + fresh `SessionPool`** through the same proxy after a
real dropped request (`tokio::time::timeout(5 ms)`), and reports whether the
operation and the precise values/observer state come back. This is a
caller-side reconnect, not a documented cancellation-recovery claim.

## What is not covered

- Only one record/class and integer payloads; no transactions, no links, no
  multi-page result sets, no live queries.
- Physical-stream reuse is not controllable: the pool keeps two connections, so
  a stale reply can be masked if the survivor gets the other stream. This is a
  property of 0.6.0's layering, recorded rather than worked around.
- `max_concurrency`/pool-size effects above 1, worker counts above 1, network
  resets/kills, and any server-side delay primitive (none used) are not covered.
- No M1-residue and no upstream issue search for this family (zero result).
