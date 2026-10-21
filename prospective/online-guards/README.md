# online-guards

Runnable mechanisms and real-target experiments for cancellation-safe resource
hand-off.  The crate is a worktree of the Dropwise research repository; it does
not depend on the offline exploration crates at runtime.

## What is here

- `src/guard.rs` — per-object ledger: `ObjId`, `Epoch`, phases
  (`Ready`/`Busy`/`Suspect`/`Recovering`/`Retired`), tickets armed before the
  first I/O, typed `Discharge` proofs, an exclusive `RecoveryPermit`, and
  standing obligations (for example an open PostgreSQL transaction).
- `src/pool.rs` — async pool whose return path routes suspect sessions to a
  bounded quarantine and a recovery worker; checkout only hands out `Clean`
  sessions; shutdown drains and retires.
- `src/proto/sky.rs` — Skytable Skyhash2 adapter and codec; recovery plans
  `discard_local`, `drain`, `drain_slow`, `finish_write_drain`.
- `src/proto/pg.rs` — PostgreSQL 3.0 simple-query adapter: status-aware
  `drain_reset`, `rollback`, `discard`, `finish_drain_reset`.
- `src/lab/` — raw baselines, fault proxy, action harness, JSONL records.
- `src/bin/` — `sky_lab`, `rule_lab`, `throughput`, `sky_probe`, `pg_probe`.

`research/online/DESIGN.md` states the contracts and invariants;
`research/online/RESULTS.md` reports the measurements, including what failed.
`research/online/data/` keeps the raw records.

## Running

```bash
cargo test
./scripts/with-skytable.sh bash -c './target/debug/sky_lab "$SKY_PORT" ../../research/online/data/sky-trials.jsonl 10'
./scripts/with-skytable.sh bash -c './target/debug/rule_lab "$SKY_PORT"'
./scripts/with-skytable.sh bash -c './target/debug/throughput "$SKY_PORT" 3 8 50'
./scripts/with-postgres.sh bash -c './target/debug/pg_probe "$PG_PORT"'
```

Both scripts start disposable loopback containers and remove them on exit.
The labs never touch a non-loopback address.

## Labels used in the results

- **scripted cut** — the adapter is told to stop after an exact byte count.
  It reaches states that are hard to hit naturally, and proves the guard's
  state machine; it is not evidence about natural cancellation rates.
- **natural cancellation** — the caller's future is dropped on a timer while
  the peer's reply is delayed or lost by a local proxy.  The proxy is fault
  injection, and the report says so.
- **real target** — Skytable 0.8.4 and PostgreSQL 18.4, both in local
  containers, with the wire bytes checked against the protocol.
- Unknown, timeout, and observation gaps are reported as failures or
  unresolved, never as safety.
