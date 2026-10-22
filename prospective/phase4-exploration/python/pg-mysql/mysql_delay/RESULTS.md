# MySQL async recovery delay and re-cancellation (round-5, group 2)

**Scope.** `mysql-connector-python` 26.7.0 pure-Python async (`mysql.connector.aio`)
against the existing real MySQL 9.6.0 on `127.0.0.1:32922`. One direct connection
per scenario, no pool. Two builds: **original** (venv copy) and **patched**
(`fix/patched`, automatic direct-close on cancellation). Synthetic delay only:
a loopback TCP proxy holds server->client chunks for **50 ms** or **5 s**. No
server failure is injected and no server variable is changed.

**Artifacts.** [run.sh](run.sh), [case.py](case.py),
[delay_proxy.py](delay_proxy.py), [analyze.py](analyze.py); full run in
[`logs/run.DdAPby/`](logs/run.DdAPby/): [records.jsonl](logs/run.DdAPby/records.jsonl)
(75 records), [summary.txt](logs/run.DdAPby/summary.txt),
[analysis.json](logs/run.DdAPby/analysis.json),
[environment.txt](logs/run.DdAPby/environment.txt),
[commands.txt](logs/run.DdAPby/commands.txt), per-case logs. All timings are
monotonic-clock milliseconds from the **first** cancel request unless noted.
`a2` is from the second cancel.

## Method

Target actions (all on one connection):

- `begin`: `start_transaction()`; cancel at the BEGIN reply. No outer write is
  issued in this target cell.
- `savepoint`: `start_transaction()` + outer parameterized `REPLACE` (`outer`
  row) + `SAVEPOINT dw_r5_sp` (legitimate nested scope inside an outer
  transaction), then cancel at the SAVEPOINT reply.
- The proxy is armed to hold the reply of the target action. The driver waits
  until the proxy has actually read that chunk, then calls `Task.cancel()`.
  Every one of the 75 targets was cancelled while its reply was held; **none**
  was delivered before the cancel (`stale_reply.delivered_before_cancel=false`
  in all records).

Recovery strategies:

- `rollback`: `connection.rollback()`.
- `reset_session`: `connection.reset_session()` (`is_connected()` PING +
  `COM_RESET_CONNECTION` + post-connection `SET NAMES`/autocommit round trips).
- `close_reconnect`: caller-level `close()` then `aio.connect()`; not a
  connector built-in.
- `savepoint_rollback_commit` (nested only): `ROLLBACK TO SAVEPOINT dw_r5_sp`
  then `COMMIT`.

Re-cancel: the recovery is run as an `asyncio` task. The driver waits for a
**real suspension** (a held server reply, or a forwarded recovery command whose
stream is blocked behind the held target reply), then cancels the recovery task.
If recovery completed before a suspension, no second cancel is issued and the
result is `unrealized` (not "ignored").

Measured durations:

- **a**: cancel request -> caller observes cancellation.
- **b**: cancel request -> transaction closed or connection evicted. Kind is
  reported; `unknown` means no closure observed, not zero. b is the first
  *observed* closure: P_S sampling is 20 ms (short) / 100 ms (long), so the
  server may have closed earlier than the reported b.
- **c**: cancel request -> successor (same handle; or new handle for
  `close_reconnect`) completes a parameterized write (`rowcount` 1/2), reads
  back `[[7]]`, **and** an independent direct observer connection sees the
  successor row. Otherwise c is `unrealized`.
- **conn**: target connection closed (proxy close, or cleanup close when the
  handle is still held).
- **occupancy**: target connection close minus cancel; for no-escalation cells
  this is `cleanup_close_ms`.

Short cells: 2 reps, medians below. Long 5 s cells: 1 rep (stated). The short-delay
run also runs an automatic escalation (close + fresh connect) after a failed
primary successor to quantify the fallback; this is **not** counted as primary
c. One long explicit escalation is reported separately as `recancel+esc`.

## Results

### Short 50 ms, top-level BEGIN (medians of 2)

| build | strategy | a | b obs. (kind) | recovery | primary c | fallback c |
|---|---|---:|---:|---:|---|---:|
| original | rollback | 0.03 | 41.5 (tx_closed) | 50.5, returns OK | **unrealized** | 679.1 |
| original | reset_session | 0.05 | 83.0 (tx_closed) | 254.6, returns OK | **unrealized** | 859.8 |
| original | close_reconnect | 0.05 | 1.6 (evicted) | 345.4 | 501.9 | - |
| patched | rollback | 0.12 | 0.14 (evicted) | 0.10, InterfaceError | **unrealized** | 499.3 |
| patched | reset_session | 0.14 | 0.16 (evicted) | 0.09, OperationalError | **unrealized** | 529.0 |
| patched | close_reconnect | 0.14 | 0.16 (evicted) | 370.4 | 527.8 | - |

Cancellation return is **0.02-0.23 ms** across all short records and
**0.03-0.34 ms** across long records; the delay does not delay the caller. At 50 ms the original's recovery consumes the
held target reply, reports OK, and leaves the stream shifted: the primary
successor sees `rowcount=0`, readback `[]`, while the observer has already seen
the successor row (84 ms). The transaction was observed closed at 41.5 ms
(the ROLLBACK command was sent at <1 ms; b is sampling-limited) but the
client-visible operation is wrong, so c is unrealized. Patched closes at
cancel (0.14 ms), recovery fails fast, and a usable successor needs a full
reconnect: 499-529 ms, not O(1).

### Long 5 s, top-level BEGIN (1 rep)

| build | strategy | a | b obs. (kind) | recovery | primary c | target conn close |
|---|---|---:|---:|---:|---|---:|
| original | rollback | 0.06 | 203.2 (tx_closed) | 5,000, returns OK | **unrealized** | 15,153.6 |
| original | reset_session | 0.07 | 5,174.7 (tx_closed) | 25,003 | **unrealized** | 35,156.6 |
| original | close_reconnect | 0.04 | 1.6 (evicted) | 35,021 | 50,028.5 | 1.9 |
| patched | rollback | 0.15 | 0.17 (evicted) | 0.16, InterfaceError | **unrealized** | 1.1 |
| patched | reset_session | 0.15 | 0.17 (evicted) | 0.16, OperationalError | **unrealized** | 1.0 |
| patched | close_reconnect | 0.18 | 0.20 (evicted) | 35,021 | 50,030.4 | 1.7 |

The 5 s target reply is held at cancel. In the original the proxy delivers it
5,000 ms later, then the recovery's own reply queues behind it. Thus
`rollback()` returns OK after 5 s having consumed the **stale BEGIN reply**;
the ROLLBACK command was sent at <1 ms and the transaction was observed closed
at 203 ms (100 ms sampling). The primary
successor then reads the next queued reply as its write result
(`rowcount=0`, `[]`), while the observer sees its row at 5,174 ms: a silent
server-side write that the client reports as failure. c is unrealized, and the
poisoned connection remains occupied for **15.15 s** until cleanup.

`reset_session()` involves five reply-bearing round trips (PING, RESET, and
three post-connection commands), hence 25 s recovery. RESET closes the transaction at
5.17 s (observed; 100 ms sampling), but recovery is still not usable: the
successor gets `rowcount=0`/`[]` and c is unrealized. The target connection stays open **35.16 s**.

Patched evicts at cancel (0.17 ms), recovery raises immediately, and no primary
successor is possible. A usable successor requires reconnect: **35 s** to
connect plus 15 s for write/read => **c = 50.03 s**. Close itself is 0.7-1.6 ms,
so close is fast; successor acquisition is not O(1)/zero-wait.

### Re-cancel at a real suspension (long 5 s unless noted)

| build | target | strategy | second cancel | b obs. (kind) | c | occupancy |
|---|---|---|---|---|---:|---:|
| original | begin | rollback | 50 ms, `stream_blocked`; ROLLBACK already forwarded; action + caller see cancel | 202.4 (tx_closed) | **unrealized** | 10,155.8 |
| original | begin | reset_session | 50 ms, `stream_blocked`; PING forwarded; action + caller see cancel | **Unknown** | **unrealized** | 10,153.2 |
| original | begin | close_reconnect | greeting held; action + caller see cancel; no handle | 1.8 (evicted) | **unrealized** | 2.2 |
| original | begin | close_reconnect + explicit retry | as above; retry close+connect after cancel | 1.7 (evicted) | 50,032.1 (fallback) | 2.2 (fallback conn 50,182.9) |
| patched | begin | rollback / reset | recovery completed in 0.16 ms; **no second cancel issued** | 0.14 / 0.18 (evicted) | **unrealized** | 1.0-1.4 |
| patched | begin | close_reconnect | greeting held; action + caller see cancel; no handle | 0.14 (evicted) | **unrealized** | 1.9 |
| original | savepoint | rollback | 50 ms; action + caller; outer still uncommitted | 151.8 (tx_closed) | **unrealized** | 10,152.1 |
| patched | savepoint | rollback | recovery completed; no second cancel | 0.37 (evicted) | **unrealized** | 1.4 |
| original/patched | savepoint | close_reconnect | greeting held; no handle | 1.4 / 0.22 (evicted) | **unrealized** | 1.9-2.0 |

Across all records, 24 second cancels were realized; in all 24 the recovery
action saw the cancellation and the caller got `CancelledError` (a2 median
0.06 ms). Eleven re-cancel cells were unrealized because recovery had already
finished; all 11 are patched rollback/reset, and no second cancel was issued
there.

Original rollback re-cancel is nuanced: ROLLBACK was forwarded before the
second cancel, so the transaction was observed closed at 202 ms, but recovery was
cancelled and the caller never got a trustworthy completion. The successor
still reads stale data and is not usable. Original reset re-cancel is worse:
only PING (not RESET) was sent, so the server transaction stays **ACTIVE** and
b is **Unknown**; the connection remains occupied 10.15 s and the successor
write is invisible (rolled back at cleanup). The local `in_tx` flag was already
false, demonstrating client/server divergence.

Patched direct close has no recovery window for rollback/reset, so a re-cancel
cannot interrupt it. For close_reconnect it can interrupt the handshake, but
there is no handle left; c is unrealized. The explicit retry after that
cancelled handshake costs 50.03 s under 5 s delay.

### Nested SAVEPOINT target (long 5 s, 1 rep)

The target starts 10 s before cancel (BEGIN and outer write are each delayed
5 s); table times are relative to cancel.

| build | strategy | b obs. (kind) | recovery | outer row | lock count | c |
|---|---|---:|---:|---|---|---|
| original | rollback | 144.6 (tx_closed) | 5,000, OK | **never visible** | 1 -> 0 | **unrealized** |
| patched | rollback | 0.20 (evicted) | 0.07, InterfaceError | **never visible** | 1 -> 0 | **unrealized** |
| original | savepoint_rollback_commit | 5,120.4 (tx_closed) | 10,001, OK | **visible at 5,120 ms** | 1 -> 0 | **unrealized** |
| patched | savepoint_rollback_commit | 0.16 (evicted) | 0.20, InterfaceError | never visible | 1 -> 0 | **unrealized** |
| original | close_reconnect | 1.8 (evicted) | 35,026 | never visible | 1 -> 0 | 50,033.0 |
| patched | close_reconnect | 0.21 (evicted) | 30,021 | never visible | 1 -> 0 | 45,030.3 |

`performance_schema.data_locks` shows exactly one lock for the outer `REPLACE`
while the transaction is active, and zero after rollback/close. Full
`rollback()` and direct close both roll back the **whole outer transaction**:
the legitimate outer row is never visible. `ROLLBACK TO SAVEPOINT; COMMIT`
preserves and commits it (visible at 5.12 s), but the client still misreads the
successor (`rowcount=0`, `[]`) and c is unrealized. Direct close prevents the
outer commit entirely. Nested close+reconnect gives a usable successor, but
only after 45-50 s under 5 s delay.

### Direct-close policy and stale replies

- **Eviction:** patched target socket is disconnected at cancel
  (`post_cancel.socket_connected=false`); b = 0.14-0.37 ms short,
  0.14-0.37 ms long; the proxy observed target connection close at
  0.7-2.9 ms (explicit `close()` itself 0.7-1.6 ms).
- **Original:** for same-connection recovery (`rollback`, `reset_session`)
  the target socket remains connected; the held reply is delivered 50 ms /
  5,000 ms after cancel and consumed as the recovery result. For original
  `close_reconnect` the target chunk is `proxy_cancelled` instead because the
  connection is closed.
- **Patched:** the held target chunk is `proxy_cancelled`, never delivered;
  no stale reply is available to recovery or to the successor.
- **Counts:** of 75 targets, 25 original kept-connection cells delivered the
  held reply after cancel, 13 original close cells cancelled it, and all 37
  patched cells cancelled it. Thus every target delay was interrupted
  before delivery; in the original kept-connection cells the stale bytes were
  delivered later, not at cancel time.
- Recovery latency: patched rollback/reset = 0.07-0.52 ms (all sub-ms); original
  rollback = 50 ms / 5 s; original reset = 255 ms / 25 s.
- Reconnect is the expensive part: 345-370 ms (50 ms delay), 30-35 s (5 s
  delay). Successor c is 480-530 ms short and 45-50 s long. **Not O(1) and not
  zero-wait.**
- For the patched close path, close is 0.7-1.6 ms but does not make the
  successor usable; reconnect + fresh write/read is required.
- Eviction is a client-connection event; server-side transaction cleanup can
  lag. In cells where a `tx_closed` transition was sampled, close/direct-close
  showed the server transaction gone at ~42 ms (50 ms delay) or ~146-202 ms
  (5 s delay), while b is the earlier connection eviction. Top-level patched
  rollback/reset cells did not catch a tx transition (b_kind=evicted only);
  nested patched cells did (~145-153 ms).
- A patched reconnect cancelled during the handshake closed its half-open
  socket ~0.31 ms after the second cancel; the original's abandoned reconnect
  socket was GC-closed ~0.22 ms after cancel. No persistent leak was observed,
  but the original has no deterministic close guarantee.

### Resource occupancy

The transaction can be closed while the connection remains occupied:

- original rollback long: tx observed closed 203 ms, connection occupied
  **15.15 s**.
- original reset long: tx closed 5.17 s, connection occupied **35.16 s**.
- original reset re-cancel: b Unknown, connection occupied **10.15 s**; server
  transaction stayed ACTIVE until cleanup.
- original rollback re-cancel: tx observed closed 202 ms, connection occupied
  **10.16 s**.
- patched direct close: connection closed 0.7-2.9 ms; no lingering handle.
- Nested original full rollback: tx closed 145 ms, connection occupied 15.15 s.

This is why foreground cancel-return latency alone is misleading. There is no
connector pool or background recovery worker in this setup: the recovery call is
caller-driven in a task, and "queued" means the proxy/connection stream is
serialized behind the held target reply. There is no pool "next borrow"; the
next user is the successor attempt on the same handle (or a fresh handle for
`close_reconnect`). No pool-capacity data is claimed.

## Unknown / unrealized and limitations

- **Unknown b:** original reset re-cancel; no tx closure was observed, and the
  transaction remained active until cleanup. This is not a zero.
- **Unrealized c:** every primary successor that returned `rowcount=0`/`[]` or
  had no handle. Escalation/retry results are labeled separately and are not
  primary c.
- **Unrealized second cancel:** patched rollback/reset recovery completed before
  any suspension (0.16 ms); no second cancel was issued.
- **Synthetic protocol delay, not server failure.** The proxy holds
  server->client chunks; the server is healthy. The 5 s delay is per chunk.
  Its server->client pipe is serialized: while a chunk is held, later replies
  are not read, so a 5 s delay on one reply queues all later replies. This is a
  service-time/queueing model, not independent one-way delay. Consequences:
  reset is 25 s, reconnect handshake is 30-35 s, and recovery replies sit behind
  the stale target reply. Do not interpret those as connector timers.
- One direct connection, no pool; no pool capacity or borrow behavior tested.
- Short = 2 reps (medians); long = 1 rep. Sampling is 20 ms (short) / 100 ms
  (long), so b has sampling resolution; connection-close timings come from the
  proxy and are precise. Long cells are bounded by a 300 s per-process timeout;
  none hit it.
- No event-loop shutdown, multi-thread, TLS, compression, or server-failure
  testing. No general log was enabled (`general_log=0`, `log_output=FILE`);
  performance-schema consumers were already enabled.
- Original reconnect cancellation has no deterministic close in the connector;
  in the observed run the abandoned socket was collected and the proxy saw the
  client close ~0.22-0.31 ms after cancel. This is a CPython refcount effect,
  not a close guarantee; do not infer a leak or a deterministic cleanup.
- Nested long cells include 10 s of pre-cancel target delay (BEGIN + outer
  write); total process seconds are therefore larger than the table's
  cancel-relative values.

## Reproduction

```bash
cd /home/ava/Desktop/Dropwise/prospective/phase4-exploration/python/pg-mysql
PY=/home/ava/dropwise-py-work-r3/venv313/bin/python \
PORT=32922 ./mysql_delay/run.sh
```

The script creates and drops a unique database, records exact commands in
`logs/run.*/commands.txt`, and runs original/patched in separate processes with
`PYTHONPATH` selecting `fix/patched` for the patched build. Environment:
Python 3.13.13, mysql-connector-python 26.7.0, MySQL 9.6.0,
`REPEATABLE-READ`, Linux x86_64; connector paths and server settings are in
[`environment.txt`](logs/run.DdAPby/environment.txt). No tracked file was
modified.

**Bottom line:** cancellation return is immediate, but with mysql-connector's
original async path a 5 s reply delay leaves a stale reply that can make
rollback/reset report success while the successor is wrong (and can silently
commit a write the client reports as failed). The patched direct-close policy
makes eviction prompt and deterministic, eliminates the stale-reply false
success, and correctly rolls back the outer transaction, but it does **not**
make a successor usable: reconnect + fresh round trips are required, costing
~0.5 s at 50 ms delay and ~45-50 s at 5 s delay. Re-cancelling recovery is
possible in the original stream-blocked state and during patched reconnect, but
neither yields a usable successor; patched rollback/reset has no recovery
window left to cancel.
