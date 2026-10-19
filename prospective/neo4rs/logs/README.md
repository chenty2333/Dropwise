# Raw logs

Final 3 × full-ladder logs for the numbers in `../RESULTS.md`:

- `release-1..3.log`: neo4rs 0.8.0 from crates.io, 10 tests (N1/N2 ×
  B1/B2/B3/M/M-residue), Neo4j 5.26.0 community. All three rounds have the same
  plan/violation counts.
- `patched-1..3.log`: the scenario crate rebuilt with
  `../patches/neo4rs/neo4rs-0.8.0-cancelled-exchange.diff` applied to a copy of
  neo4rs 0.8.0 (`[patch.crates-io]`). All three rounds have the same counts.
- `probe-60ms.log`, `probe-120ms.log`: manual timing probes from
  `../tests/probe.rs`. The first cancels before the RUN reply is read (residue
  24 B, the target write is not committed, `v = 0`); the second cancels before
  the DISCARD reply is read (residue 96 B, the write committed, `v = 1`). Both
  make the next `RETURN 'fresh'` through the same pool return no row.

`environment.txt` records the image tag/digest and server/driver versions.

The LADDER lines carry the variant in brackets; `LADDER-VIOLATION` lines carry
the plan as `#target@pending:race`, so M and B3 can be compared plan by plan.
Probe logs are diagnostic evidence for the trigger positions in section 4.3 of
`RESULTS.md`; they are not counted in the result table.
