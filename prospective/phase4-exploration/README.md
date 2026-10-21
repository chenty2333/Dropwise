# Phase 4 exploration (unfrozen)

Started 2026-10-21. This directory is a small, time-boxed first contact with
real Rust clients after the standalone-crate productization. Round 2 was added
after reading the round-1 results; it is a follow-up on the same three clients,
not a new client batch, not a Phase 4 expansion, and not a preregistered study.
It does **not** change or freeze the formal Phase 4 draft list, gates or budget.

The latest transaction-entry, recovery and pool-handoff results are in
[ROUND4-RESULTS.md](ROUND4-RESULTS.md), including the scope of integration review.
[ROUND3-RESULTS.md](ROUND3-RESULTS.md) and the
[adapter audit](ADAPTER-AUDIT-ROUND3.md) retain the preceding all-client findings
and separate valid observations from old invalid runs. [RESULTS.md](RESULTS.md) and the description below retain
the rounds 1–2 context; they are not the current all-client summary.

## First batch (written before any cancellation run)

Three Rust client families from the existing Phase 4 main list. No ecosystem
rescan and no client outside that list.

| # | family / version | reuse mode | primary operation | local service |
|---|---|---|---|---|
| 1 | `bolt-client 0.11.0` + `bb8-bolt 0.8.0` (`bb8 0.8.6` resolved) | pooled, **and** direct `Client<Stream>` | `RUN` + `PULL` of one write query on the same node | Neo4j 5.26.0 community, one disposable container, loopback only |
| 2 | `async-imap 0.12.0` | direct `Session` held outside the target future | `SELECT INBOX` (session state); survivor appends to the same mailbox and reads it back | GreenMail 2.1.14, disposable container, loopback only |
| 3 | `async-smtp 0.10.2` | direct `SmtpTransport` held outside the target future | one complete `send`; survivor sends a second message on the same transport | GreenMail 2.1.14, same kind of container, loopback only |

Why these three:

- One family (bolt-client) is used in both a pooled and a direct shape, so the
  same client and server let us compare a pool hand-off with same-owner reuse.
- The other two are direct handles with different protocol state: IMAP has
  tagged commands and a selected mailbox; SMTP has a multi-step envelope and a
  DATA terminator. Both are chosen from the existing R-H list, not because of a
  known cancellation implementation.
- Neo4j is already used by the earlier `prospective/neo4rs` work and its image
  is local. GreenMail provides SMTP, IMAP and a REST mailbox view, so the
  independent observer can bypass the client and the proxy. Both services bind
  `127.0.0.1` on ephemeral ports.
- The first operation on each path is a complete, publicly documented request
  with an effect that an independent observer can see. We do not start by
  enumerating every API.

The concrete client versions were queried on 2026-10-21 from crates.io; the
oldest-but-current line constraints are recorded in each client directory.

## Shape of each experiment

B1/B2/B3/M1 as in the existing four-level protocol, three repetitions each.
Every level shares one setup, one target, one Pending mechanism and one
`Config`:

- B1: cancel, then stop; only panic, timeout and obligation leaks.
- B2: after cancellation, repeat the same operation with the same arguments and
  check only `Ok`.
- B3: after cancellation, run a different operation of the same kind on the
  same reuse unit and check only `Ok`.
- M1: `explore_observed(Expect::Atomic)`; survivor does real work on the same
  object, and an observer that bypasses the tested client/pool/proxy records
  the concrete result.

Before the ladder: a no-cancellation run, a reuse/reconnect diagnostic, a
pre/post calibration check, and a check that the observer sees the effect. A
zero without these is not evidence.

Pending is produced without modifying the client: `dropwise::net::Proxy`
holding every server-to-client chunk for a fixed real-time delay
(`Flavor::MultiThread`). Any extra check is recorded as an added exploration, not folded into the
original result.

## Running

Each client directory has its own `Cargo.toml`, `run.sh` and `logs/`.
The runner uses a disposable loopback-only container and removes it on exit.
No external account or host service is used.

```sh
bash prospective/phase4-exploration/clients/bolt/run.sh
bash prospective/phase4-exploration/clients/imap/run.sh
bash prospective/phase4-exploration/clients/smtp/run.sh
```

## Round 2 (added after reading round 1)

Round 2 keeps the same client versions and services. It adds only the checks
needed to resolve questions left by round 1:

- SMTP: an M1 variant that records concrete SMTP reply codes, a third send on
  the same transport, a bounded (2 s) send, a debug-level command/reply trace of
  the timeout cut, and a dependency-copy guard patch compared with the published
  crate. The original ladder and its logs are unchanged.
- IMAP: cancel SELECT, retry SELECT, then APPEND, SEARCH/FETCH and the REST
  observer; a dependency-copy APPEND-continuation patch with regression tests;
  three repetitions of the affected ladder.
- BOLT: the prior bb8 0.8.6 `is_valid` guard on a separate vendored copy, tested
  side by side with the registry baseline; three repetitions, an isolated
  checkout cancellation, a real replay of one failed plan, and normal/validation-
  failure path checks. No unrelated crate is upgraded.

```sh
bash prospective/phase4-exploration/clients/smtp/run-round2.sh
bash prospective/phase4-exploration/clients/smtp/run-round2-fixed.sh
bash prospective/phase4-exploration/clients/imap/run-round2.sh
bash prospective/phase4-exploration/clients/bolt/run-round2.sh
```

The `round2-*` logs are separate from the round-1 logs. SMTP and IMAP
patches are applied to dependency copies outside the tested crate; the BOLT
fixed variant is a separate Cargo project with a vendored bb8 0.8.6. These are
experimental contrasts, not proposed upstream patches.

## Status

This file is the starting point, not the report. Actual observations, added
explorations, cost, limitations and next steps are in `RESULTS.md`. The complete
round-2 evidence is in each client's `RESULTS-FRAGMENT.md` and `logs/round2-*`.

## Runner ownership and review checks (2026-10-21)

Runners now create a fresh `logs/run.XXXXXX/` directory and print its path;
existing round-1/round-2 output is not overwritten. `LOG_DIR` can select a parent
for these run directories. `ROUND2_WORK` / `ROUND2_TEST_WORK` likewise select a
**parent**, not a directory to erase. Only a newly created private child is
removed on exit. Signal handlers exit rather than continuing after cleanup.
SMTP runners distinguish an expected violation exit with a valid report from a
build failure, panic, failed calibration or outer timeout, and propagate failures.

No-service runner checks: `python3 clients/test_runners.py` (from this directory).
SMTP rejection/poison-policy checks: `bash clients/smtp/run-rejections.sh`.
The poison patch is an experiment, **not a production-ready fix**: it also
rejects reuse after fully consumed ordinary negative replies. IMAP patch checks
now use `append-continuation.review.patch`; the original round-2 patch remains
for interpreting the historical logs. The review patch preserves unsolicited
notifications and stops on BYE. See RESULTS.md §9 for reviewed outcomes.
