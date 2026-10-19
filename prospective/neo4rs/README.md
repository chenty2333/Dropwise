# neo4rs 0.8.0 dynamic verification

Paired-scan follow-up for the `deadpool` manager + `neo4rs::Connection` pairing
in `../candidate-scan/PAIRING.md` section 4: `recycle()` sends one RESET and
`reset()` accepts the first `SUCCESS`, so a cancelled exchange can leave a
stale `SUCCESS` that is consumed as RESET's acknowledgement.

One independent Cargo project, one integration test per target operation:

- `tests/neo4rs.rs` — N1 (pooled write `SET k.v = $v`) and N2 (pooled read of
  two tagged rows, `fetch_size = 1`), each at B1, B2, B3, M and M-residue.
  Shared reporting from `../support/ladder.rs`.
- `tests/probe.rs` — manual timing probe (not part of the ladder): cancel one
  pooled write at a chosen wall-clock instant, print the proxy residue and what
  the next read through the same pool returns.
- `patches/neo4rs/` — triage-only cancellation guard; applied to a copy of
  neo4rs 0.8.0 through `[patch.crates-io]`, never to the detection build.

## Run

From the repository root:

```sh
bash prospective/neo4rs/run-neo4rs.sh                    # release, 3 reps
VARIANT=patched bash prospective/neo4rs/run-neo4rs.sh    # patch contrast
REPS=1 bash prospective/neo4rs/run-neo4rs.sh
```

The runner starts one disposable `neo4j:5.26.0-community` container, publishes
Bolt only on `127.0.0.1`, exports `DW_NEO4J_URL`, saves the raw output under
`logs/`, and removes the container on exit. The pinned digest is recorded in
`RESULTS.md`; the runner warns if the tag now resolves to a different digest.
`DW_NEO4J_URL` skips the container and uses an existing server.

## Levels

Every level uses the same `Config` (`Flavor::MultiThread { workers: 1 }`,
`races = [Immediate, AfterWake]`, `max_cancellations = 1`, 3 repetitions) and
the same Pending mechanism: `dropwise::net::Proxy` with `reply_delay = 50 ms`.
The pool has `max_connections(1)`, so the survivor's borrow is the cancelled
operation's connection.

- B1: stop after the cancellation.
- B2: retry the same operation with the same arguments; judge `Ok` only.
- B3: run a different operation of the same kind; judge `Ok` only.
- M: `explore_observed(Expect::Atomic)`; the survivor writes `v = 7` to the
  same node through the same pool, reads it back through the same pool, reads a
  unique fresh tag through the same pool, and a separate `Graph` (not through
  the proxy or the pool) confirms `v = 7`.
- M-residue: M plus `Proxy::residue()` after a 100 ms wait before the survivor.

`RESULTS.md` has the numbers, triage, trigger positions, the patch contrast and
the upstream issue search. `logs/README.md` maps raw logs to numbers.
