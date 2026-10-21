# Online guards: measurements

All numbers below come from local containers on the same host.  They are
measurements of this implementation against these baselines, not production
claims.  Raw records: `research/online/data/`.  Scripted cuts, natural
cancellation (proxy-delayed/lost replies), and real-target runs are labelled
separately; a zero observation is not a completeness proof.

## Review and integration (2026-10-22)

The coordinator corrected the synchronous-repair publication window in the
other workstream and independently reviewed this implementation. Here, pool
shutdown now closes admission and worker publication under the state lock;
shutdown itself drains queued sessions even if a worker has not started. The
pool enforces the recovery deadline even for an adapter that never returns.
Queue overflow is not double-counted as an eviction, shutdown refusal is not
queue overflow, validation rejection is not acquire cancellation, and shutdown
reports queue drains for that call rather than the pool's lifetime.

The rule study no longer consumes injected delay/loss labels or planned byte
states. It reads byte state from the interrupted client before executing an
action, checks the intended cut was reached, and rejects missing/inconsistent
observations. The experiment checks effect authority before completing a write.
Sky trials record `mode` and `valid_cut`; invalid cuts prevent aggregate claims.

All B0/B1/B2 clients below are **local strategy models using RawSky**, not the
upstream Skytable crate or its actual patch. The peer is a real Skytable server.
The PostgreSQL probe likewise uses the local adapter/naive client, not psycopg
or asyncpg. These experiments establish mechanism behaviour, not superiority
over upstream client implementations.

Integration validation: 41 Rust tests passed (38 library + 3 rule-input tests).
The independent PostgreSQL probe completed; 330 Skytable trials all reached
valid cuts, and guarded/retire/drain strategies returned the correct successor
in every trial. The immediate-reuse model still returned stale replies in
S1/S2/S6/S7, errors in S3/S4 and timeouts in S8. The corrected 135-action rule
study completed without observation gaps. In the final 3-second, 8-worker load
rerun, raw drain reached 78,634 ops/s and guard+pool 70,971 ops/s, both with zero
stale replies/errors/reconnects; guard recovery completed 4,256 attempts. This
is a single local strategy comparison, not an upstream-client benchmark.
The large tables below retain the original measured run for historical context;
they are not relabelled as new measurements.

## 1. Skytable 0.8.4

Workload per trial: prime `a=111` and `b=222`; perform one operation whose
reply is cut/cancelled; return the connection; issue a successor select of
`a`, which must return 111.  `stale=222` is a silent misassociation (the
successor received the abandoned operation's reply).  `err` means the
successor got an operational/server error; `timeout` means no reply.  The
scripted-cut scenarios cut at a fixed byte; S6/S7/S8 are natural cancellations
through a local proxy that delays the reply by 20 ms/300 ms or discards it on
the first connection.

Baselines:

- **B0 raw reuse** — local model of immediate reuse; no protection.
- **B1 raw retire** — local model of boolean poison/retire: reconnect
  on any suspect object.
- **B2 raw drain** — consume one reply frame (120 ms), reconnect if the drain
  fails; no guard.
- **B3 guard** — this work: byte-observation policy + quarantine + recovery
  worker; `discard_local` when nothing was written, `drain` when a reply is
  pending, `retire` for a partial write or an unrecoverable stream.
- **B3f finish** — B3 plus the `AtomicEffectMayApply` contract, which permits
  finishing the exact interrupted insert.

| scenario | baseline | n | unsafe | ok | err | timeout | recovered | reconnects | p50 us | p95 us |
|---|---|---|---|---|---|---|---|---|---|---|
| S1_cut_before_read | B0_raw_reuse | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 139 | 521 |
| S1_cut_before_read | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 190280 | 199696 |
| S1_cut_before_read | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 458 | 933 |
| S1_cut_before_read | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 672 | 1078 |
| S2_cut_mid_reply | B0_raw_reuse | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 21 | 37 |
| S2_cut_mid_reply | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 189692 | 199658 |
| S2_cut_mid_reply | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 292 | 487 |
| S2_cut_mid_reply | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 483 | 936 |
| S3_partial_insert | B0_raw_reuse | 10 | 0 | 0 | 10 | 0 | 0 | 0 | 41020 | 42461 |
| S3_partial_insert | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 186112 | 186876 |
| S3_partial_insert | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 307906 | 320433 |
| S3_partial_insert | B3_guard | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 186407 | 195993 |
| S3_partial_insert | B3f_finish | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 123619 | 124501 |
| S4_insert_before_read | B0_raw_reuse | 10 | 0 | 0 | 10 | 0 | 0 | 0 | 258 | 440 |
| S4_insert_before_read | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 185472 | 187289 |
| S4_insert_before_read | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 675 | 1069 |
| S4_insert_before_read | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 918 | 1579 |
| S5_nothing_written | B0_raw_reuse | 10 | 0 | 10 | 0 | 0 | 0 | 0 | 393 | 507 |
| S5_nothing_written | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 186068 | 188850 |
| S5_nothing_written | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 306912 | 310471 |
| S5_nothing_written | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 688 | 1118 |
| S6_late_reply_caught | B0_raw_reuse | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 18065 | 18671 |
| S6_late_reply_caught | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 208526 | 216472 |
| S6_late_reply_caught | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 41005 | 41451 |
| S6_late_reply_caught | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 41065 | 42487 |
| S7_late_reply_beyond_first_timeout | B0_raw_reuse | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 299177 | 299565 |
| S7_late_reply_beyond_first_timeout | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 487932 | 501087 |
| S7_late_reply_beyond_first_timeout | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 610824 | 627829 |
| S7_late_reply_beyond_first_timeout | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 601376 | 603051 |
| S8_reply_lost | B0_raw_reuse | 10 | 0 | 0 | 0 | 10 | 0 | 0 | 801471 | 802190 |
| S8_reply_lost | B1_raw_retire | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 187036 | 196991 |
| S8_reply_lost | B2_raw_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 307696 | 308511 |
| S8_reply_lost | B3_guard | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 1516139 | 1519263 |

Reading the table:

- B0 confirms the hazard is real on the wire: 10/10 successors received the
  abandoned `b=222` reply in S1/S2/S6/S7; S3/S4 produced server errors
  (`-6`/`-108`), and S8 timed out.  Immediate reuse fails these interrupted-stream cases, although S5 is a
  legitimate no-write control and succeeds.
- B1 is safe everywhere but reconnects in every scenario, including S5 where
  the byte count says nothing left the process.  That is the measured false
  eviction: ~188 ms per event, 10/10 trials.
- B2's drain is enough when a reply is already buffered or arrives within
  120 ms (S1/S2/S4/S6: 0 reconnects) and is not enough when nothing was
  written (S5: drain waits 120 ms then reconnects), when the write is partial
  (S3: no reply to drain, then reconnect) or when the reply is later than the
  budget (S7/S8: reconnect).
- B3 uses the byte state to avoid B2's S5 eviction (`discard_local`, ~0.3 ms)
  and B1's S1/S2/S4/S6 evictions (drain, 0 reconnects).  For S7 it drains,
  retries with a slower drain, and recovers without a reconnect; the wall
  time (600 ms) is *worse* than B1's 488 ms because this host's reconnect
  (~188 ms) is faster than waiting out the 300 ms reply.  The gain is zero
  connection churn, which matters when reconnect is expensive; the table does
  not claim a latency win here.
- S8 is the honest cost of bounded patience: three attempts (drain, slow
  drain, retire) take 1.52 s before the successor runs, versus 0.19 s for B1.
  If the prior says "lost" rather than "slow", B1's choice is better; the
  guard makes the trade-off explicit and measurable instead of hiding it.
- B3f is the only baseline that preserves the interrupted insert's effect:
  `finish_write_drain` completes the exact request and the side check reads
  333; B3 retires and the side effect is absent.  This difference is an
  authority decision (`AtomicEffectMayApply`), not a protocol improvement.

## 2. PostgreSQL 18.4

Real server, trust authentication, simple-query protocol.  The probe log is
`research/online/data/pg-probe.log`.

The central hazard is not a shifted *byte* but a shifted *transaction*.  A
client that cancels a `BEGIN` after writing it, then reuses the connection,
reads the `BEGIN` reply as the reply to its next statement.  In the probe the
naive client sent `BEGIN`, then `INSERT` without reading the `BEGIN` reply:

```
naive after INSERT request: tag="BEGIN" status=T
naive row after close: 0
```

The insert was executed inside a transaction the client did not know about;
closing the connection rolled it back.  The client saw a successful command
tag and lost the row.

The guarded path on the same fault:

```
guard begin: cut ... phase=Suspect
guard successor while suspect: refused (suspect (cancelled) since operation #1)
server state before recovery: idle in transaction
guard recovery: Recovered(pg.ready_idle) ... post-recovery server state: idle
guard autocommit insert: command="INSERT 0 1" status=idle
guard row present: 1
```

The recovery drained the abandoned `BEGIN` reply (status `T`), issued
`ROLLBACK`, observed `Z` with `I`, and only then minted the witness.  The
successor was refused before recovery; the server's own `pg_stat_activity`
confirmed the open transaction existed and was cleaned.

Other observed cases:

| case | observation | outcome |
|---|---|---|
| completed `BEGIN` returned to the pool | `ReadyForQuery` status `T`; qualification `NeedsCleanup{open_transaction}` | `rollback` cleanup, then `Clean`; subsequent autocommit insert persisted |
| cut mid-`SELECT` reply (17 bytes buffered) | `Suspect`, read=partial | `drain_reset` to `Z I`; successor saw the correct rows |
| cut `BEGIN` after 3/11 bytes, `MustNotApply` | write=partial | `Failed(partial_write_unrecoverable)`; retired; no bytes completed |
| same, `AtomicEffectMayApply` | write=partial | `finish_drain_reset` completed the exact `BEGIN`, drained `T`, rolled back to `I`; `Recovered` |
| `DISCARD ALL` while status `T` | server error `25001`, then status `E` | not a rollback substitute; adapter refuses it outside `I` |
| pool-level cut `BEGIN` | `recovery_enqueued=1 succeeded=1 failed=0` | successor insert succeeded; row present |
| cut `COMMIT` with an unread reply, plan `rollback` | `Failed(pending_exchange_requires_drain)`, retired | no false success; no successor on a shifted stream |
| same, plan `drain_reset` | `Recovered(pg.ready_idle)`, standing None, status `I` | successor `INSERT 0 1` on the same connection |

## 3. Cross-target migration

The ledger (identity, epoch, ticket armed before I/O, exclusive permit,
witness) transferred to PostgreSQL unchanged.  The policy and recovery layer
did not:

| concern | Skytable | PostgreSQL |
|---|---|---|
| frame boundary | one complete reply frame, no extra bytes | messages until `ReadyForQuery` |
| session status | none | `I` / `T` / `E`; `T`/`E` is a standing obligation |
| recovery command | none; drain and reuse, or retire | `ROLLBACK`; `DISCARD ALL` only from `I` |
| partial write | retire unless the effect contract permits finishing | same, but finishing a `BEGIN` then rolling back is meaningful |
| late completion | positional reply; drain it or retire | status/reply of the abandoned exchange; drain then reconcile |
| false eviction risk | B1 reconnects when nothing was written | a boolean "dirty" flag would roll back a clean idle session |
| rule count in the adapter policy | 4 plans (`discard_local`/`drain`/`drain_slow`/`retire`) | 4 plans (`drain_reset`/`rollback`/`discard`/`retire`) |

The important negative result: a single "drain one frame" plan is not a
cross-protocol recovery abstraction.  PostgreSQL required a second phase
(`ROLLBACK`) and a standing obligation that outlives the operation; Skytable
required byte-level state to avoid reconnecting when nothing was written.
The guard's *state machine* is shared; the *recovery plan* is protocol
specific.

## 4. Rule study: observed inputs, not fault-oracle inputs

The original `data/rule-lab.log` and `data/rule-table-split-error.txt` are
historical runs of a flawed experiment: features included configured
`delay=20ms/300ms/lost` and hard-coded intended byte states. They are **not**
evidence that these facts can be observed online. Their tables and categorical
"overfitting" story are superseded by this section.

The corrected executable runs each of 9 states × 5 actions × 3 repetitions
against a real server (135 action trials). Features are actual `write`/`read`
state captured before recovery, plus declared `contract` and `op`. No delay,
loss, scenario name, action result, or future completion is an input. Missing
observations, unrealized cuts, unexpected operation errors and inconsistent
states across actions stop the experiment rather than becoming labels.
Completing a request is screened against its effect authority before execution.

The existing sparse/mid/full training sets and three holdout states remain
unchanged. Holdout is only a same-protocol scenario split, not an independent
library or deployment. Each policy is evaluated against the measured action
table; learned trees are not installed in the production path of the guard.
Safety here means correct successor association and the declared completion
authority, not a complete application specification.

Removing the oracle exposes a real information limit: delayed and permanently
lost replies can have identical decision-time observations. Per-state cheapest
actions can disagree even when the feature vector is identical. A static
classifier therefore cannot reproduce all those cost-optimal labels. A recovery
policy may acquire information by bounded waiting; that is an action, not a
fact available before acting.

The integration rerun still shows the S5 counterexample teaching the no-write
reuse rule. It does **not** establish superiority to a simple drain or hand-written
policy. The full training set selected retirement for complete writes in this
run and unnecessarily reconnected on all three holdouts. More samples did not
repair the missing information. Costs and exact leaves are timing-sensitive;
this small experiment is an action-label feasibility study, not general rule
synthesis or feature discovery.

Reproduce with `cargo build --bins` then
`./scripts/with-skytable.sh bash -c './target/debug/rule_lab "$SKY_PORT"'`
from `prospective/online-guards`. Regression tests require identical features
for 20 ms, 100 ms, 300 ms and lost-reply cases with the same observed byte state,
and distinguish actual partial writes from planned complete-write cuts.


## 5. Load and tail latency

Eight concurrent workers, 3 s, one `select` per operation; every 50th
operation per worker is a scripted cut-before-read of a `b=222` select, and
the next `a=111` select must not receive 222.  B3 latency includes
acquire/return.  No warmup; loopback; scripted faults at roughly 2% of
operations per worker.

| mode | ops | ops/s | stale | err | faults | reconnects | p50 | p95 | p99 | max |
|---|---|---|---|---|---|---|---|---|---|---|
| b0 raw reuse | 416,231 | 138,630 | 1,171 | 8,311 | 8,319 | 0 | 51 us | 104 us | 141 us | 1.5 ms |
| b1 raw retire | 5,250 | 1,647 | 0 | 0 | 105 | 105 | 56 us | 327 us | 207 ms | 227 ms |
| b2 raw drain | 232,359 | 77,440 | 0 | 0 | 4,644 | 0 | 91 us | 151 us | 184 us | 1.0 ms |
| b3 guard | 205,519 | 68,485 | 0 | 0 | 4,107 | 0 | 102 us | 179 us | 235 us | 225 ms |

B3's pool snapshot: `connects=8 retired_suspect=0 recovery_ok=4107
failed=0 unresolved=0 queue_full=0`.

- B0 is the fastest and delivers 1,171 silent 222 replies plus 8,311 errors;
  its throughput is not a usable number.
- B1 is safe but collapses to 1,647 ops/s because every fault forces a
  reconnect; the p99 is 207 ms and the run completes only 5,250 operations.
- B2 and B3 both keep zero reconnects under this fault schedule; B3 costs
  about 12% throughput relative to B2 (68,485 vs 77,440 ops/s) for the guard,
  quarantine, and recovery worker, in these local strategy implementations. This does not measure upstream-library speedups.
- B3's p99 (235 us) is close to B2's (184 us), but its max is 225 ms: when
  several sessions become suspect at once, the single recovery worker
  serialises attempts and a successor can wait for the queue.  The tail is
  the availability cost of the recovery queue, not a safety failure; the
  permit is per object, so concurrent recovery tasks are a straightforward
  fix that is not implemented here.
- These are scripted faults at a high rate on loopback, not a production
  capacity claim.  The reconnect cost (~190 ms) is this host's
  authentication cost; WAN/TLS behaviour has not been measured and cannot be inferred from this ratio.


## 6. Hypotheses that did not survive

- *"A boolean poison bit plus reconnect is a sufficient online guard."*
  Safe, but not sufficient as a mechanism: it reconnects when the observation
  says nothing was written (S5) or when a reply is already buffered (S1/S2/
  S4/S6), paying the measured reconnect cost each time.  The guard's job is
  to make the cheaper safe action available, not only to prevent reuse.
- *"Drain one frame is protocol-native recovery."*  False for a reply later
  than the drain budget (S7) and for PostgreSQL `T`/`E`.  Recovery needs a
  deadline ladder and, for PostgreSQL, a reset command with its own status
  check.
- *"The server status/reply is enough; no standing obligation is needed."*
  False: a completed `BEGIN` is a clean `ReadyForQuery` exchange, yet the
  object must not be handed out without `ROLLBACK`.
- *"Completing a partially written request is a protocol repair."*  False:
  it changes the effect outcome.  Only the declared `AtomicEffectMayApply`
  contract permits it; B3 without it retires.
- *"Positional replies can be matched after reuse."*  False: B0 returns 222
  for a successor that asked for 111; in PostgreSQL the shift moves a whole
  transaction, not just a value.
- *"Offline labels alone produce cost-optimal online rules."* False in this
  experiment: identical observed states can require different cost-optimal
  actions. The configured delay/loss is not an available online observation.
  Declared authority, bounded waiting and an explicit cost model remain needed.

## 7. Adversarial review and fixes

A separate review agent read the guard, pool, PostgreSQL and Skytable adapters,
and the lab, without modifying files or running tests.  It found real defects.
The safety-relevant ones were reproduced here and fixed; the rest are disclosed
below rather than hidden.

Fixed and exercised:

- **PostgreSQL recovery could acknowledge the abandoned exchange.**  A
  `rollback`/`discard` plan wrote its command and broke on the first `Z I`,
  which could be the abandoned exchange's `ReadyForQuery`; the recovery
  command's own reply was then left for the successor.  `PgConn` now tracks
  `pending` (a complete request whose reply was not read to `Z`);
  `rollback`/`discard` refuse with `pending_exchange_requires_drain`, and
  `drain_and_reset` only drains when `pending`, then rolls back if the status
  is `T`/`E`.  The probe now shows both sides:

  ```
  pending rollback: plan=rollback result=Failed(Other("pending_exchange_requires_drain")) phase=Retired
  pending drain_reset: result=Recovered(pg.ready_idle) status=Idle standing=None phase=Ready
  pending drain_reset successor: command=Some("INSERT 0 1") status=Idle
  ```

- **A successful `drain_and_reset` left the standing transaction obligation
  set.**  It now clears `open_transaction` only after observing `I`; the
  rollback probe reports `standing=None qualification=Clean`.
- **An unknown `ReadyForQuery` status was accepted as complete.**  It is now a
  protocol error (and the connection is retired); startup rejects it too.
- **Skytable `discard_local` certified an I/O-broken socket.**  It now refuses
  when the abandonment cause is `Io`; a zero byte count alone is not evidence
  that the transport is alive.
- **`RecoveryPermit` marked itself finished before validating the witness.**
  A stale witness could strand the object in `Recovering` forever.  It now
  returns the object to `Suspect`, where a new claim can retry; a stale
  witness test covers it.  `Retired` is terminal under a cancelled permit and
  under the claim undo (CAS transitions, tests added), so a retired socket
  cannot be resurrected.
- **The Sky frame-consumed marker could certify a stream with trailing
  bytes.**  The marker is set only after the extra-bytes check; the
  "survives process death" wording in the design note was wrong and was
  removed.  Both adapters' markers are in-memory, per-attempt hints.
- **Pool acquire/return vs shutdown.**  `make_lease` re-checks `closed` while
  holding the state lock, `put_back` checks and pushes under the same lock,
  and `enqueue_recovery` refuses after `closed`; the tested paths no longer
  hand out a dirty session. Integration additionally closes worker publication
  and queue/worker-start interleavings with deterministic regression tests.
- **Effect authority used the connection's current contract.**  Recovery now
  uses the contract recorded in the abandoned operation's report
  (`RecoveryContext.contract`), so `with_contract` on a live connection cannot
  retroactively authorise finishing a request that was armed under
  `MustNotApply`.  A finish plan on an already-complete request no longer
  requires `MayApply`, because there is nothing left to finish.
- **PG cut snapshots were never stored**; the probe can now observe them.

Remaining measurement qualifications:

- The `unsafe` column counts only stale/empty successors.  B0's S3/S4
  successors received server errors (contaminated stream) and are shown in
  `err`, not `unsafe`; the table therefore undercounts B0's failures, while the
  stale counts are the silent ones.
- B3 `reconnects` is successful connects minus one; failed connects are
  counted separately, and there were none in these trials.  B1/B2 count
  reconnect attempts.  The two are not identical quantities.
- B3's measured latency includes the side-effect check for S3/S4 (an extra
  query); it is a small overestimate, not material at ~190 ms.
- S6-S8 are proxy-injected cancellations. Historical JSONL lacks a mode field;
  current records include it and the observed byte state. The 2 ms timer can
  fire before the intended state; current runs reject such trials explicitly.
- `last_plan` is written before execution, so the plan column is the selected
  plan, not a claim that it succeeded; `recovery` and `reconnects` carry the
  outcome.
- Shutdown and counter defects listed in the original review are repaired in
  the integration changes described above; tests exercise the exact handoff
  boundaries. This is not a formal concurrency proof.

## 8. Upstream baseline and live ownership (2026-10-22)

### 8.1 A mature upstream stack on the same fault shape

`prospective/upstream-pg-baseline` pins tokio-postgres 0.7.18 + bb8 0.9.1 +
bb8-postgres 0.9.0 and puts a loopback byte proxy between the client and a real
`postgres:18-alpine`; an independent connection reads `pg_stat_activity` and
row state.  The four modes hold a selected reply and cancel the client future.
Fresh measured output is in that directory's README.

| case | upstream observation | reading |
|---|---|---|
| typed `BEGIN` reply held, transaction future cancelled, then released | server `idle`, `query=ROLLBACK`; same backend, successor `SELECT 1` OK | the typed API already cleans up at this boundary; the guard does not claim this as new |
| same, reply held forever, pooled connection returned, `bb8` max_size=1, timeout=700 ms | `get #1` TimedOut 701.8 ms, `get #2` TimedOut 701.3 ms; pool `connections=1 idle=1`; one proxy connection | a half-open connection is not detected; the pool re-hands the wedged object instead of replacing it |
| COMMIT reply held, `tx.commit()` cancelled | server committed; independent observer sees 1 row | commit-in-doubt: cancellation is not rollback |
| manual `BEGIN`, connection returned, next checkout | `is_valid` succeeds in ~512 us; successor query runs inside the transaction | bb8-postgres validation is status-blind; checkout is not a cleanliness witness |

This is a **strong baseline**, not a straw man: at the BEGIN boundary the
upstream typed transaction passes, and a client-side "we sent a rollback" claim
would be false.  The failure class is different: after a mid-message cut the
client cannot distinguish a dead peer from a slow one, `is_closed()` stays
false, and bb8's timeout path returns the same connection to idle.  The
repository's own pool does not validate or reuse a suspect object; it
quarantines it for an explicit recovery witness or retires it.  The exact
half-open case has not yet been replayed against the guard pool end to end;
the pool's deadline/retirement behaviour is covered by deterministic tests,
not by this proxy run.  That gap is listed in the limits.

### 8.2 What the ownership witness adds, and what it does not

The guard now has live owner claims (`guard.rs`, `OwnerClaim`).  A claim is
RAII: its drop is positive evidence that the owner relinquished authority.
Nested claims are legitimate (outer transaction, inner savepoint), releasing
one does not release the others, and a handoff replaces the whole cohort and
starts a new generation.  Tests cover:

- a watchdog refused with `Owned` while a live transaction exists, then
  allowed only after every claim is released;
- an owner-authorised recovery, so a live owner is not forced to choose
  between "the watchdog cleans behind my back" and "nothing can clean";
- a forgotten claim (`mem::forget`) that keeps blocking; it is not inferred
  dead, and the pool defers then retires the object after its ownership grace;
- a second cancellation during recovery, retirement while an owner is live,
  cross-object/stale witnesses, and a handoff during an in-flight operation
  being refused until the operation settles or is abandoned;
- the pool parking a returned session with a live sub-owner, reaping it when
  the claim is released, and never counting it as a clean return.

The measured part is the decision path and the pool bookkeeping (unit and
integration tests with a fake session).  The protocol adapters do not yet
create claims for their own transactions, and there is no kernel/runtime hook:
a claim held by another process, or a task killed without unwinding, is not
observed.  The design therefore treats an absent claim as *unknown*, not as
"no owner": a session with an obligation and no claim is cleaned only by an
explicit recovery plan, and a leaked claim costs the object after a bounded
grace.

### 8.3 The synchronous Python contrast, briefly

The committed synchronous `psycopg` study remains the real-target evidence for
why server state is not an ownership witness: after an interrupted entry the
server is `idle in transaction` and a retry can silently nest as a savepoint
(60 trials: 21 clean, 12 D, 9 C, 18 B; all non-clean runs lost the write).  A
fresh rerun reproduced the committed contrast and field records, with only
xid/line drift.  A scratch probe showed that a dropped `transaction()` context
manager is still a live generator that can send `ROLLBACK`, and that GC timing
changes during exception handling; a weakref/GC liveness witness is therefore
not authority.  The Python repair was not re-implemented with owner tokens in
this round; the Rust RAII claims are the implementation of that idea.

## 9. Limits


- Faults are injected by a local proxy or exact byte cuts; natural
  cancellation rates are not measured.
- All servers are loopback containers; reconnect cost (~190 ms) is dominated
  by authentication and is not a WAN/TLS number.
- PostgreSQL coverage is simple-query + trust auth only; extended protocol,
  COPY, and cancellation via `CancelRequest` are not exercised.
- The multiplexed adapter is designed (per-stream generation + connection
  framing guard) but not implemented; the cross-mechanism claim is limited to
  the two single-connection protocols above.
- Deadlines and attempt limits are fixed constants, not derived from a
  calibrated cost model.
- The rule study is small; it demonstrates label construction and holdout
  behaviour, not a general synthesis method.
- The review was static and found defects that the original probes did not
  exercise; the fixes above are covered by unit tests and the PG probe.  The
  Sky/load and PostgreSQL probes were independently re-run during integration;
  the rule study required a separate methodological correction (§4).
