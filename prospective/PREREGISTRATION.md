# Prospective study: pre-registration (v1, 2026-09-24)

**Status: frozen before any Dropwise run against the targets below.** Changes after the
freeze are recorded in `AMENDMENTS.md` with a date and reason, and results obtained under an
amendment are reported separately from results under this version.

## 1. Question

Given only contracts that the target projects **themselves** state, and scenarios written
before running the tool, does Dropwise find violations of those contracts, at what cost, and
how many of its reports are false alarms?

This complements the historical reproductions (`../repro/`), whose contracts were written
after reading each defect report.

## 2. Target selection (done before the freeze)

Eligibility:
1. Tokio-based async Rust library, actively released.
2. **Not used in designing Dropwise or the codebook**: not one of the 27 survey
   repositories, not the source of any reproduction case.
3. **States a cancellation contract in its own documentation or source comments**, so the
   contract does not come from us.
4. Can be exercised in-process or against a local server.

Screened (source of each crate's latest release searched for cancel-safety statements):

| Crate (version) | Statement found | Decision |
|---|---|---|
| tokio-tungstenite 0.30.0 | public docs (`src/lib.rs` "Cancel safety") | **target T1** |
| bb8 0.9.1 | source comment (`src/inner.rs`) | **target T2** |
| sqlx-core 0.9.0 | internal, conditional (`src/net/socket/buffered.rs`) | secondary, exploratory (T3) |
| mongodb 3.9.1 | README: dropping driver futures is not supported | excluded: drop is outside the contract |
| fred 10.1.0 | command-timeout option "more cancellation-safe", "not perfect" | excluded: no contract for dropped futures |
| async-nats 0.50.0, deadpool 0.13.1, lapin 4.12.0, rumqttc 0.25.1, tokio-postgres 0.7.18, async-channel 2.5.0, flume 0.12.0, kanal 0.1.1, tower 0.5.3 | none | excluded: no stated contract |
| tokio-util 0.7.19 | many | excluded: lives in the tokio repository (survey) |

Disclosure: bb8 appeared once in the survey data, as a cross-referenced PR (djc/bb8#161) on
redis-rs#325; it was not studied or used for design. The tester (the Claude session that
built Dropwise) has read only the contract passages quoted below, not the targets'
implementations.

Pinned versions (crates.io checksums):

| Crate | Version | Checksum |
|---|---|---|
| tokio-tungstenite | 0.30.0 | 17a073bfed563fa236697a068031408a93cd9522e08abf9933ead3e73411bd71 |
| tungstenite | 0.30.0 | e48ac77174b19c110a50ab2128b24215ac9cb40e0e12e093fb602d175c569d22 |
| bb8 | 0.9.1 | 457d7ed3f888dfd2c7af56d4975cade43c622f74bdcddfed6d4352f57acc6310 |
| sqlx-core | 0.9.0 | 05b44e85bf579a8eeb4ceaa77a3a523baf2bf0e9bac7e40f405d537b5d2d5ccb |

Dropwise is used as of the commit that contains this file, with no changes during the study
except bug fixes, which are logged and trigger a re-run of all affected scenarios.

## 3. Contracts (verbatim) and their operationalisation

### T1: tokio-tungstenite 0.30.0

> "Reading messages is cancel-safe. `WebSocketStream` has no dedicated read methods; messages
> arrive through its `Stream` implementation, and reading a message via `StreamExt::next`
> follows that trait's cancel-safety: if the `next()` future is dropped before it resolves
> (for example, as a branch of `tokio::select!` that another branch completes first), no
> message is lost. The next poll resumes from the same position in the stream."
> "The `Sink` side (sending) does not carry a documented cancel-safety guarantee."
> (`src/lib.rs`, lines 192–202)

**C1.** For any sequence of messages sent by the peer, a reader that drops `next()` futures at
arbitrary Pending boundaries and keeps reading receives exactly the sent data messages, in
order, with no loss and no duplication, and the stream stays usable.

Scenarios (all over an in-process connection; client-side reader in the program's own
`select!` via `ctx.race`, competitor = `Preempt`):
- S1.1 small text and binary messages;
- S1.2 messages larger than the read buffer (≥ 256 KiB);
- S1.3 fragmented messages (continuation frames);
- S1.4 the peer writes byte-by-byte (every frame split across many reads);
- S1.5 control frames interleaved (ping; the reader must answer with pong while reading);
- S1.6 close frame at the end: after the close handshake the stream ends cleanly.

Out of contract: the `Sink` side. Any sending-side finding is recorded as "no contract",
never as a violation.

### T2: bb8 0.9.1

> "Cancellation safety: make sure to wrap the connection in a `PooledConnection` before
> allowing the code to hit an `await`, so we don't lose the connection."
> (`src/inner.rs`, lines 93–94)

**C2.** Cancelling `Pool::get()` at any Pending boundary never loses a connection or pool
capacity: after the cancelled call and the settle phase, (a) `max_size` concurrent `get()`
calls all succeed within the configured `connection_timeout`, and (b) every connection the
manager created is either held by the pool or was dropped by the pool itself; none is leaked
(tracked with Dropwise obligations on a test `ManageConnection`).

Scenarios (test `ManageConnection` whose `connect` and `is_valid` suspend):
- S2.1 idle connection available (`test_on_check_out` on and off);
- S2.2 no idle connection, pool below `max_size` (a new connection must be created);
- S2.3 pool at `max_size`, caller queued waiting for a connection;
- S2.4 `is_valid` fails, the pool must discard and replace the connection;
- S2.5 two cancellations in one run (`max_cancellations = 2`).

### T3 (secondary, exploratory): sqlx-core 0.9.0

> "Cancel-safe as long as the callback does not modify the passed `BytesMut`"
> (`src/net/socket/buffered.rs`, line 67; an internal API)

Because the statement is internal, T3 checks a public consequence only: **C3.** after a
Postgres query future (`sqlx::query(..).fetch_*`) on a pooled connection is dropped at a
Pending boundary, later queries through the same pool return their own results (no response
mismatch). Server: `postgres:16` in docker. T3 findings are reported separately as
exploratory, because the contract used is our reading of an internal comment.

## 4. Procedure

1. For each target, write all scenarios listed above **before the first run**, commit them,
   and record the commit in `JOURNAL.md`.
2. Configuration: `Config::default()` plus `races = [Immediate, Reschedule(1), After(1ms)]`,
   `max_cancellations = 1` (2 for S2.5), `settle = Settle::default()`,
   `scenario_timeout = 10 s`, `Flavor::CurrentThread`. Each scenario is run 3 times.
3. Every report with a violation is triaged in this order:
   a. **Harness or scenario error**: the violation disappears when the scenario is corrected
      so that it matches the contract text. Counted as a false alarm, with the cause.
   b. **Out of contract**: the behaviour is outside the quoted contract (e.g. the Sink side).
   c. **Contract violation**: reproduced in 3 of 3 runs, reduced to a minimal scenario, and
      explained by a root cause in the target's source.
4. Contract violations are reported upstream **only with the project owner's approval**
   (this repository's owner decides), with the minimal scenario. The maintainer's response
   and date are recorded; the outcome is classified at the cut-off date (section 6).
5. Scenario or contract changes after the first run are allowed only to fix scenario
   mistakes (3a); each is logged with the reason, and results before and after are both kept.

## 5. Measures (reported for every target, including null results)

- Scenarios written; lines of scenario code; wall-clock time to write them.
- Plans explored, Pending boundaries per target, unrealized / unsettled plans, run time.
- Reports: false alarms (3a), out-of-contract (3b), contract violations (3c).
- For violations: reproduction rate, minimal scenario size, maintainer response.
- Sensitivity check (positive control, fixed in advance): for T2, a vendored copy of bb8
  0.9.1 with the `PooledConnection` wrapping moved after the `await` that the comment warns
  about must make C2 fail. If it does not, the T2 scenarios are reported as insensitive.
  (No positive control is defined for T1 before reading its implementation; one may be added
  by amendment, reported as post-hoc.)

## 6. Stopping rules and cut-off

- Time budget: one working day of tester time per primary target, half a day for T3.
- A null result (no violation) is a result and is reported with the coverage measures.
- Cut-off for maintainer responses: 2026-10-24.

## 7. Recording

- `JOURNAL.md`: append-only, timestamped entries (scenario commits, runs, triage decisions,
  amendments).
- `runs/<target>/<scenario>/<n>.txt`: full Dropwise report of every run.
- The SHA-256 of this file at freeze time is recorded in `FREEZE.txt`.
