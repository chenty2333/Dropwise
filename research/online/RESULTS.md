# Online guards: measurements

All numbers below come from local containers on the same host.  They are
measurements of this implementation against these baselines, not production
claims.  Raw records: `research/online/data/`.  Scripted cuts, natural
cancellation (proxy-delayed/lost replies), and real-target runs are labelled
separately; a zero observation is not a completeness proof.

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

- **B0 stock** — immediate reuse; no protection.
- **B1 poison** — the stock fix's boolean poison + `PoisonOnDrop`: reconnect
  on any suspect object.
- **B2 drain** — consume one reply frame (120 ms), reconnect if the drain
  fails; no guard.
- **B3 guard** — this work: byte-observation policy + quarantine + recovery
  worker; `discard_local` when nothing was written, `drain` when a reply is
  pending, `retire` for a partial write or an unrecoverable stream.
- **B3f finish** — B3 plus the `AtomicEffectMayApply` contract, which permits
  finishing the exact interrupted insert.

| scenario | baseline | n | unsafe | ok | err | timeout | recovered | reconnects | p50 us | p95 us |
|---|---|---|---|---|---|---|---|---|---|---|
| S1_cut_before_read | B0_stock | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 139 | 521 |
| S1_cut_before_read | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 190280 | 199696 |
| S1_cut_before_read | B2_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 458 | 933 |
| S1_cut_before_read | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 672 | 1078 |
| S2_cut_mid_reply | B0_stock | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 21 | 37 |
| S2_cut_mid_reply | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 189692 | 199658 |
| S2_cut_mid_reply | B2_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 292 | 487 |
| S2_cut_mid_reply | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 483 | 936 |
| S3_partial_insert | B0_stock | 10 | 0 | 0 | 10 | 0 | 0 | 0 | 41020 | 42461 |
| S3_partial_insert | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 186112 | 186876 |
| S3_partial_insert | B2_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 307906 | 320433 |
| S3_partial_insert | B3_guard | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 186407 | 195993 |
| S3_partial_insert | B3f_finish | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 123619 | 124501 |
| S4_insert_before_read | B0_stock | 10 | 0 | 0 | 10 | 0 | 0 | 0 | 258 | 440 |
| S4_insert_before_read | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 185472 | 187289 |
| S4_insert_before_read | B2_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 675 | 1069 |
| S4_insert_before_read | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 918 | 1579 |
| S5_nothing_written | B0_stock | 10 | 0 | 10 | 0 | 0 | 0 | 0 | 393 | 507 |
| S5_nothing_written | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 186068 | 188850 |
| S5_nothing_written | B2_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 306912 | 310471 |
| S5_nothing_written | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 688 | 1118 |
| S6_late_reply_caught | B0_stock | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 18065 | 18671 |
| S6_late_reply_caught | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 208526 | 216472 |
| S6_late_reply_caught | B2_drain | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 41005 | 41451 |
| S6_late_reply_caught | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 41065 | 42487 |
| S7_late_reply_beyond_first_timeout | B0_stock | 10 | 10 | 0 | 0 | 0 | 0 | 0 | 299177 | 299565 |
| S7_late_reply_beyond_first_timeout | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 487932 | 501087 |
| S7_late_reply_beyond_first_timeout | B2_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 610824 | 627829 |
| S7_late_reply_beyond_first_timeout | B3_guard | 10 | 0 | 10 | 0 | 0 | 10 | 0 | 601376 | 603051 |
| S8_reply_lost | B0_stock | 10 | 0 | 0 | 0 | 10 | 0 | 0 | 801471 | 802190 |
| S8_reply_lost | B1_poison | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 187036 | 196991 |
| S8_reply_lost | B2_drain | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 307696 | 308511 |
| S8_reply_lost | B3_guard | 10 | 0 | 10 | 0 | 0 | 0 | 10 | 1516139 | 1519263 |

Reading the table:

- B0 confirms the hazard is real on the wire: 10/10 successors received the
  abandoned `b=222` reply in S1/S2/S6/S7; S3/S4 produced server errors
  (`-6`/`-108`), and S8 timed out.  It is never safe, even though its p50 is
  the lowest.
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

## 4. Rule study (offline action labels)

The action lab runs every (state, action) three times against the real server.
A state is a byte-level snapshot after a fault: `write`/`read`
(none/partial/complete), a delay bucket, the effect contract, and the
operation.  An action is `reuse`, `drain` (120 ms), `drain_slow` (1.2 s),
`finish_drain` (complete the exact request, then drain), or `retire`
(reconnect).  Safety is the successor select returning 111; cost is the
median action latency plus 150 ms per reconnect in the penalised variant; a
false eviction is a reconnect when some safe action needed none.

The nine states include three **holdout** states (complete-mid-reply, insert-
complete, and a 100 ms delay) that are never in the training set.  S5
("nothing written") is a counterexample probe: it is in the `mid`/`full`
training sets, not in `sparse`.

| training set | rules (penalty 0) | holdout cost | unsafe | false evictions | probe S5 |
|---|---|---|---|---|---|
| sparse (S1,S3) | 2 | 199,524 us | 0 | 0 | retire, 189 ms, **1 false eviction** |
| mid (+S5) | 3 | 199,524 us | 0 | 0 | reuse, 284 us, 0 |
| full (+S6,S7,S8) | 6 | 288,613 us | 0 | **1** (S9) | reuse, 284 us, 0 |
| retire_all | 1 | 667,244 us | 0 | 3 | retire |
| drain_only | 1 | 199,524 us | 0 | 0 | drain, 308 ms, **1 false eviction** |
| handwritten | 4 | 200,399 us | 0 | 0 | reuse, 0 |

Learned trees:

```
sparse:  write=complete -> drain | write=partial -> finish_drain
mid:     write=complete -> drain | write=none -> reuse | write=partial -> finish_drain
full:    delay=20ms -> finish_drain | delay=300ms -> retire | delay=lost -> retire
         delay=none & write=complete -> drain
         delay=none & write=none -> reuse
         delay=none & write=partial -> finish_drain
```

At penalty 150 ms the full tree changes only the 300 ms leaf (`drain_slow`);
its holdout cost rises to 438,613 us because the penalty makes the S9
retirement expensive.

What this shows:

- The byte state is the observation that removes the two availability errors
  the stock and drain-only policies cannot see: `write=none` makes reuse safe
  (S5, 284 us vs 189 ms), and `write=complete` with a consumed frame makes a
  drain safe (S1/S2/S4/S6).
- Adding the S5 counterexample to the offline set **adds a rule** and removes
  a measured false eviction; the rule is not invented from the protocol, it is
  selected by the counterexample and the cost model.  This is the concrete
  independent contribution of offline exploration: it tells the online guard
  which byte/status facts are worth recording and which action they select.
- The full tree **overfits categorical delay values**: a 100 ms holdout that
  was not in the table falls through to `Retire`, paying 189 ms and one
  reconnect, while a simple `drain` (and the hand-written deadline ladder)
  handles it at ~200 us.  More offline states did not help the holdout here;
  the learned rules were worse than `drain_only`.
- The 20 ms leaf selected `finish_drain`, which is safe in that state because
  the request is already complete (there is nothing left to write) and the
  cost is a near-tie (40.8 ms vs 39.7 ms for `drain`).  The tree does not know
  that finishing is only authorised by the effect contract; it would be wrong
  to read this leaf as a general rule.
- The holdout does not contain S5, so `drain_only` looks perfect there and
  fails the probe.  Reporting only the holdout would hide that; the probe is
  included for exactly that reason.

The conclusion is deliberately narrow: offline action labels can select and
justify the online observations and rules under a stated cost model, and a
counterexample changes the rule; they do not by themselves synthesise a
policy that generalises across unseen delays, and the learner's categorical
split is weaker than a hand-written deadline ladder on that axis.


## 5. Load and tail latency

Eight concurrent workers, 3 s, one `select` per operation; every 50th
operation per worker is a scripted cut-before-read of a `b=222` select, and
the next `a=111` select must not receive 222.  B3 latency includes
acquire/return.  No warmup; loopback; scripted faults at roughly 2% of
operations per worker.

| mode | ops | ops/s | stale | err | faults | reconnects | p50 | p95 | p99 | max |
|---|---|---|---|---|---|---|---|---|---|---|
| b0 stock | 416,231 | 138,630 | 1,171 | 8,311 | 8,319 | 0 | 51 us | 104 us | 141 us | 1.5 ms |
| b1 poison | 5,250 | 1,647 | 0 | 0 | 105 | 105 | 56 us | 327 us | 207 ms | 227 ms |
| b2 drain | 232,359 | 77,440 | 0 | 0 | 4,644 | 0 | 91 us | 151 us | 184 us | 1.0 ms |
| b3 guard | 205,519 | 68,485 | 0 | 0 | 4,107 | 0 | 102 us | 179 us | 235 us | 225 ms |

B3's pool snapshot: `connects=8 retired_suspect=0 recovery_ok=4107
failed=0 unresolved=0 queue_full=0`.

- B0 is the fastest and delivers 1,171 silent 222 replies plus 8,311 errors;
  its throughput is not a usable number.
- B1 is safe but collapses to 1,647 ops/s because every fault forces a
  reconnect; the p99 is 207 ms and the run completes only 5,250 operations.
- B2 and B3 both keep zero reconnects under this fault schedule; B3 costs
  about 12% throughput relative to B2 (68,485 vs 77,440 ops/s) for the guard,
  quarantine, and recovery worker, and is 42x B1.
- B3's p99 (235 us) is close to B2's (184 us), but its max is 225 ms: when
  several sessions become suspect at once, the single recovery worker
  serialises attempts and a successor can wait for the queue.  The tail is
  the availability cost of the recovery queue, not a safety failure; the
  permit is per object, so concurrent recovery tasks are a straightforward
  fix that is not implemented here.
- These are scripted faults at a high rate on loopback, not a production
  capacity claim.  The reconnect cost (~190 ms) is this host's
  authentication cost; on a WAN/TLS link the B1 collapse would be worse and
  B3's zero-reconnect path more valuable.


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
- *"Offline labels alone produce the online rules."*  Partly false: byte
  state separates discard/drain/finish/retire, but delayed vs lost needs a
  delay observation, and the choice between waiting and reconnecting depends
  on a reconnect-cost input that is not in the state.

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
  hand out or strand a session after shutdown.  A narrow queue/worker-start
  interleaving in `shutdown` remains possible and is listed below.
- **Effect authority used the connection's current contract.**  Recovery now
  uses the contract recorded in the abandoned operation's report
  (`RecoveryContext.contract`), so `with_contract` on a live connection cannot
  retroactively authorise finishing a request that was armed under
  `MustNotApply`.  A finish plan on an already-complete request no longer
  requires `MayApply`, because there is nothing left to finish.
- **PG cut snapshots were never stored**; the probe can now observe them.

Disclosed, not fixed:

- The `unsafe` column counts only stale/empty successors.  B0's S3/S4
  successors received server errors (contaminated stream) and are shown in
  `err`, not `unsafe`; the table therefore undercounts B0's failures, while the
  stale counts are the silent ones.
- B3 `reconnects` is successful connects minus one; failed connects are
  counted separately, and there were none in these trials.  B1/B2 count
  reconnect attempts.  The two are not identical quantities.
- B3's measured latency includes the side-effect check for S3/S4 (an extra
  query); it is a small overestimate, not material at ~190 ms.
- S6-S8 are proxy-injected cancellations.  The scenario name and this report
  say so; the JSONL records do not carry a mode field, and the 2 ms cancel
  timer can fire before the write completes (the intended state is asserted,
  not re-observed from the cut snapshot in the rule lab).
- `last_plan` is written before execution, so the plan column is the selected
  plan, not a claim that it succeeded; `recovery` and `reconnects` carry the
  outcome.
- Shutdown: an enqueue that lands between the queue drain and the worker
  shutdown can still be refused/retired but not served; the narrow race is
  noted rather than claimed impossible.
- Counter/report definitions (`evictions`, `ShutdownReport.drained_queue`,
  `Hold::drop`) still have the review's edge cases; the tables above use the
  explicit fields, not those composite counters.

## 8. Limits

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
  Sky and load tables were re-run on the fixed code; the rule-lab table was
  collected with the raw baselines and is unaffected by the guard/pool fixes.
