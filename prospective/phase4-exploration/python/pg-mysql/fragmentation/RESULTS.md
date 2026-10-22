# RESULTS — finite half-frame delivery and send backpressure (group 3)

Status: exploratory, 2026-10-22 (Asia/Tokyo), not Phase 4 data. New files only
under `prospective/phase4-exploration/python/pg-mysql/fragmentation/`; the
existing cases, proxy, logs and reports were not edited. No upstream contact.

## 0. Question and answer in one paragraph

Does delivering PostgreSQL replies in finite pieces — inside the 5-byte frame
header, inside the payload, or at complete-message boundaries — increase the
number of target suspensions `K`, or expose responsibility states that the
normal chunk-delay proxy does not? For psycopg 3.3.6 transaction entry,
**yes to `K`, no new server->client responsibility state in this sample**:
`K` rises from 6 (no split) to 17 (boundary/payload) or 21 (header), but every
cut still aggregates by transaction phase — `enter` 5/5 Neither/INTRANS,
`body` 11/11 Pre/IDLE, `exit_commit` 5/5 Post/IDLE. A separate
send-direction experiment **did find a new, reproducible failure**: cancelling
psycopg while its large request is only partly written raises
`TypeError: an integer is required` instead of `CancelledError`, leaves the
connection `ACTIVE`/busy, and makes every later statement fail with
`OperationalError: another command is already in progress`.

## 1. Environment and commands

| item | value |
|---|---|
| Python | CPython **3.13.13** (`/home/ava/dropwise-py-work-r3/venv313/bin/python`) |
| psycopg | **3.3.6** (binary implementation; `PSYCOPG_IMPL=python` unavailable: no system libpq) |
| asyncpg / psycopg_pool | installed but **not used** in this group |
| server | fresh disposable `postgres:18-alpine`, PostgreSQL **18.4**, container `dw-frag-g3-1792647451`, `127.0.0.1:32890` |
| network | loopback only; tested connection through `FragmentingProxy`; observer direct, never through the proxy; SSL disabled (`sslmode=disable`) so every new connection is v3-framed |
| tables | fresh unique table per scenario, `(k text primary key, v integer)`; values target=1, survivor=7, retry/next=2 or 5; dropped/discarded with the connection |

Commands (from `prospective/phase4-exploration/python/pg-mysql`):

```bash
export PYTHONPATH=".:/home/ava/Desktop/Dropwise/research/python-retro"
python -m fragmentation.run_small --port 32890 \
  --out fragmentation/logs/run.1792647665
python -m fragmentation.run_small --port 32890 \
  --out fragmentation/logs/run.bp.1792647809 --only bp
python -m fragmentation.min_repro --port 32890
```

`run_small.py` imports `cases.txphase_case.CmScenario` read-only and swaps the
`DelayProxy` module global at runtime (`tx.DelayProxy = FragmentingProxy`) so
the existing target/survivor/observer shell is reused without editing it.
`run.1792647498` and `run.1792647581` are aborted harness attempts (attribute
and JSON-emit errors); they are not used below.

## 2. Proxy modes and what is counted

`fragment_proxy.py` subclasses `DelayProxy`. It reads only the v3
type/length header to find message boundaries; this is not a general parser.
For each complete server→client message it does **one** finite split and a real
`asyncio.sleep(0.15)` while the rest is withheld:

| mode | bytes written before the pause | then |
|---|---|---|
| `off` | unchanged `DelayProxy` chunk forwarding, no pause | — |
| `boundary` | the whole message | pause, next message |
| `header` | 3 bytes (type + 2 of the 4 length bytes) | pause, rest |
| `payload` | 7 bytes (5-byte header + 2 payload bytes); payload-less messages fall back to boundary | pause, rest |

Counters (`snapshot()`) record bytes and writes in each direction, number of
frames and pauses, the current message type/length/split/sent bytes, and the
last splits. `pause_active` is true only during the real pause. The proxy does
**not** claim that a `write` call is a TCP segment; the client-side evidence in
§4 is what supports "partial delivery".

## 3. `K` over repeated normal runs (psycopg 3.3.6, transaction CM)

`K` is the `_PhasedTask` yield count for the uncancelled target
(`async with` entry → upsert → exit commit, driven manually by
`CmScenario._cm_write`). Three fresh runs per mode; target only, setup
excluded. Server→client bytes are identical (72) in all modes; only the
writes/pauses change.

| mode | K (3 reps) | target seconds | s2c writes | s2c bytes | pauses | s2c frames |
|---|---:|---:|---:|---:|---:|---:|
| off | 6, 6, 6 | 0.305 | 3 | 72 | 0 | 0 (not parsed) |
| boundary | 17, 17, 17 | 1.256–1.257 | 9 | 72 | 9 | 9 |
| header | 21, 21, 21 | 1.513–1.514 | 18 | 72 | 9 | 9 |
| payload | 17, 17, 17 | 1.255–1.256 | 12 | 72 | 9 | 9 |

The nine frames are the extended-protocol replies of the three commands:
`BEGIN` → `C`,`Z`; upsert → `1`,`2`,`n`,`C`,`Z`; `COMMIT` → `C`,`Z`.
Client→server is 195–196 bytes in 3 writes in every run.

`K` depends on the pause being long enough for the client to run between
fragments. In an earlier prototype with `pause=0.05` (not the authoritative
run) boundary was K=9 and header K=12, versus 17/21 at 0.15 s. The increase is
therefore a real-pause/scheduling effect, not a function of the proxy's write
count alone. The fixed variant also had K=21 in 3/3 header runs.

## 4. Client-side evidence for partial delivery

Uncancelled target, mode armed after setup; a watcher samples during the first
pause (the `C` message of the `BEGIN` reply), 50 ms after the pause starts.
Two reps per mode.

| mode | frame at sample | s2c written | writes | task suspended | future | client `recv_q` | client `PQisBusy` | client tx status |
|---|---|---:|---:|---|---|---:|---|---|
| header | `C` len 11, cut 3, sent 3 | 3 | 1 | yes | pending | 0 | true | ACTIVE |
| header | same | 3 | 1 | yes | pending | 0 | true | ACTIVE |
| payload | `C` len 11, cut 7, sent 7 | 7 | 1 | yes | pending | 0 | true | ACTIVE |
| payload | same | 7 | 1 | yes | pending | 0 | true | ACTIVE |

Interpretation: the proxy has written only a 3- or 7-byte prefix of an 11-byte
message, the client kernel receive queue is empty (the prefix was consumed by
the process), libpq reports no complete result (`PQisBusy`), and the target
task is suspended on a pending future. That is a partial read followed by
suspension, not merely multiple proxy writes. The target then completed with
K=21 (header) / 17 (payload), result `ok`.

## 5. Cut outcomes by phase (normal, header mode, every discovered `k`)

One cut per `k`, 21/21 realized, 0 Unknown, 0 unrealized. Calibration:
two no-target and two target-completed observations agreed
(`pre_ok=True`, `post_ok=True`, `pre != post`).

| k range | phase | observed cut location (sent/len) | verdict | server after unwind | client after unwind |
|---|---|---|---:|---|---|
| 1 | enter | no frame yet (`current=None`) | Neither | idle in transaction | INTRANS |
| 2–3 | enter | `C` 3/11 | Neither | idle in transaction | INTRANS |
| 4–5 | enter | `Z` 3/6 | Neither | idle in transaction | INTRANS |
| 6 | body | `Z` 6/6 (complete) | Pre | idle | IDLE |
| 7–8 | body | `1` 3/5 | Pre | idle | IDLE |
| 9–10 | body | `2` 3/5 | Pre | idle | IDLE |
| 11–12 | body | `n` 3/5 | Pre | idle | IDLE |
| 13–14 | body | `C` 3/16 | Pre | idle | IDLE |
| 15–16 | body | `Z` 3/6 | Pre | idle | IDLE |
| 17 | exit_commit | `Z` 6/6 (complete) | Post | idle | IDLE |
| 18–19 | exit_commit | `C` 3/12 | Post | idle | IDLE |
| 20–21 | exit_commit | `Z` 3/6 | Post | idle | IDLE |

`Neither` is the known psycopg entry trap: the server is `idle in
transaction`, the client is INTRANS with `num_transactions=1`, and the retry
through the official CM succeeds while the independent observer sees no row
(`[]`). `Pre` means only the survivor row is visible; `Post` means target and
survivor are visible. The same entry verdict (`Neither`, INTRANS) occurred at
`k=1`/`k=2` for boundary, header and payload modes; boundary k=2 is a
complete-message cut, header k=2 is 3/11 bytes and payload k=2 is 7/11 bytes.

**No new server→client responsibility state appeared**: cuts inside a header
or payload produced the same state as the complete-message and no-fragment
cuts in the same phase. Outcomes aggregate by message phase in this sample.
This does **not** claim message-level cut completeness: one split per message,
one pause length, one target shape and a single cancellation were tested.

Fixed variant (`install_psycopg_enter_fix`, runtime monkeypatch only), header
mode, K=21 in 3/3 runs, entry cuts k=1,2,3: 3/3 realized, 3/3 `Pre`, client
IDLE, server idle. This is the existing experimental repair, not a new one.

## 6. Send-direction backpressure (new finding)

Target: `SELECT length(%s)` with an **8 MB** parameter, psycopg autocommit,
through the proxy; after setup the proxy forwards at most 64 KiB of the
client→server direction and stops reading the client. No `SO_SNDBUF` tuning is
used; `SO_SNDBUF` is not used as evidence. `send_q` is the Linux `SIOCOUTQ`
count on a duplicate of the client fd.

| condition | K (reps) | seconds | at sample |
|---|---:|---:|---|
| normal (no hold) | 5, 5 | 0.025–0.027 | completed, result `8000000` |
| hold, uncancelled | 7, 8 | 0.323–0.336 | task suspended; `SIOCOUTQ` 3.10/4.10 MB; proxy c2s 65,735 B (< 8 MB); `PQflush`=1; client ACTIVE; 2/2 |
| hold, then cancel at the verified suspension | — | TypeError in 0.004 s | pre-cancel `SIOCOUTQ` 3.58/4.10 MB; proxy c2s 65,536 B in 1 write; `PQflush`=1; client ACTIVE; task suspended |

After the cancel, both reps:

- outcome: `TypeError: an integer is required` (not `CancelledError`);
- connection: `closed=False`, `transaction_status=ACTIVE`, `PQisBusy=True`,
  `SIOCOUTQ=0`;
- every next statement: `OperationalError: sending query failed: another
  command is already in progress`;
- `conn.close()` returns in bounded time; after close the backend is gone and
  the earlier `SELECT` had no side effect by construction.

This is real send backpressure: the task is suspended with millions of bytes
still in the client's send queue and only 64 KiB accepted by the proxy, so the
request is partially written. The error is not caused by the cut engine: plain
`asyncio.create_task` + `task.cancel()` reproduces it (`min_repro.py`), and the
same direct cancel at the same point produces:
`psycopg/waiting.py:183 s = next(gen)` →
`psycopg_binary/_psycopg/generators.pyx:138 send` →
`TypeError: an integer is required`.

Located mechanism (psycopg 3.3.6):

1. `AsyncConnection.wait()` (`connection_async.py:531-553`) catches
   `CancelledError`; because `pgconn.transaction_status == ACTIVE` it calls
   `_try_cancel()` and then re-enters
   `waiting.wait_async(gen, ...)` **with the already-started generator**.
2. `wait_async()` begins with `s = next(gen)` (`waiting.py:183`), resuming the
   in-progress C generator with `None`.
3. The C `_send` generator expects the integer readiness mask and raises
   `TypeError`; the original cancellation never propagates as
   `CancelledError`, and no drain/close/rollback runs.

A web search found psycopg issue #1005, "asyncio.CancelledError shadowed by a
TypeError" (https://github.com/psycopg/psycopg/issues/1005), but the page could
not be fetched (non-public IP) and the mechanism was not verified as the same;
no upstream triage or novelty claim is made.

## 7. Mature positive control

Control: a plain `SELECT` on the same psycopg connection class, cancelled
without a transaction CM, so libpq/psycopg's own cancel-and-drain handles the
partial reply (the protocol-level defensive positive noted in round 3/4, not
an independent client implementation). Table rows `target=1`, `next=2`;
observer direct.

| mode | K (2 reps) | seconds |
|---|---:|---:|
| off | 1, 1 | 0.001–0.002 |
| boundary | 7, 7 | 0.453 |
| header | 9, 9 | 0.603–0.604 |
| payload | 7, 7 | 0.453 |

Cuts at k=1,2,3 for boundary/header/payload: 9/9 cancelled; successor
`SELECT ... next` returned its own `(2,)` 9/9; client stayed IDLE and open;
observer rows stayed `[[next,2],[target,1]]`. 9/9 `control_ok=True`. This
shows the fragmentation harness itself does not force a broken connection, and
that libpq's drain consumes the remainder of a partially delivered plain
reply. It does **not** absolve the transaction CM's entry bookkeeping, and it
is not an independent library control.

## 7b. Run cost

The authoritative main run is about **123 s wall time** and **84 executions**:
12 K runs (4 modes × 3), 6 entry cuts, 21 full header cuts, 4 evidence probes,
8 calibration scenarios, 3 fixed K runs, 3 fixed cuts, 8 control K runs,
9 control cuts, 4 backpressure runs and 2 backpressure cuts. Per-target cost:
0.305 s at K=6, 1.26 s at K=17, 1.51 s at K=21; body/exit cuts cost 1.06–1.52 s
because psycopg's own drain waits for the withheld remainder. The
backpressure-only rerun is ~2 s; the minimal reproduction is ~1 s.

## 8. Unimplemented / unrealized / limits

- **Unimplemented:** asyncpg transaction target (no K/ladder); boundary and
  payload full ladders (only entry k=1,2; header was full); fixed variant full
  ladder (only k=1–3); control cut wire offsets (control K and successor were
  logged, but no per-cut `current`/offset snapshot); cuts during
  COPY/pipeline/notification/password/TLS; more than one split per message;
  multiple pause lengths in the authoritative run; no `PSYCOPG_IMPL=python`
  comparison (no system libpq).
- **Unrealized cuts:** none in the reported runs — normal header 21/21,
  entry cuts 6/6, fixed 3/3, control 9/9. No cut was skipped as `Unknown` in
  the reported header ladder.
- **Not claimed:** TCP segmentation from `write()` calls; message-level cut
  completeness; a detection-rate or stability estimate; upstream newness;
  that the send-direction failure is psycopg-only or C-implementation-only.
- The positive control is protocol-level, not a separate mature client.
- `run.1792647498` and `run.1792647581` were aborted harness attempts and are
  retained as actual logs but not used as results.

## 9. Files

- `fragment_proxy.py` — proxy modes, counters, `recv_q`/`send_q`, pause and hold.
- `run_small.py` — authoritative K, ladder, evidence, fixed, control and
  backpressure runner; writes one JSONL per run.
- `min_repro.py` — minimal send-backpressure cancellation reproduction.
- `logs/run.1792647665/` — authoritative normal/fixed/control/backpressure run
  (`records.jsonl`, `stdout.log`; its bp section used the early cut sampling).
- `logs/run.bp.1792647809/` — authoritative backpressure-only rerun with the
  pre-cancel suspension sample and proxy byte/write counts.
- `logs/run.repro.1792647833/` — `min_repro.py` output incl. full traceback.
- `partial_send_fix.py` — runtime-only, fail-closed containment tested below.

## 10. Narrow containment of the partial-send defect (before/after)

This is a containment experiment, not an upstream-ready repair.  The defect is
in psycopg's cancellation wait path: a second `wait_async(gen)` call resumes an
already-started send generator with `None`, producing `TypeError: an integer is
required` and leaving the connection ACTIVE/busy.  `partial_send_fix.py`
monkeypatches `AsyncConnection.wait`; only when that exact TypeError escapes it
terminates the connection and re-raises `CancelledError`.  It does not pretend
the lease is usable.

| run | result of `task.cancel()` during blocked 8 MB send | connection after | successor |
|---|---|---|---|
| original (no patch) | `TypeError: an integer is required` (not cancellation) | ACTIVE, `PQisBusy`, not closed | every statement: `OperationalError: another command is already in progress` |
| containment | `CancelledError` | closed | explicit `OperationalError: the connection is closed` |

Commands (local PostgreSQL 18.4 at 32870):

```sh
PYTHONPATH=".:$REPO/research/python-retro" \
  ~/dropwise-py-work-r3/venv313/bin/python -m fragmentation.min_repro --port 32870
PYTHONPATH=".:$REPO/research/python-retro" \
  ~/dropwise-py-work-r3/venv313/bin/python -m fragmentation.min_repro --port 32870 --fix
```

The containment makes the interruption observable and prevents a stuck
connection from being reused, but it loses the lease: callers must acquire a
new one.  Section 11 replaces it with a source-level repair in the
wait/cancel path; the containment is retained only as the earlier experiment.
No upstream contact was made and no novelty claim is based on an unfetched
issue page.

## 11. Source-level partial-send cancellation repair (`codex/partial-send-cancel`)

The containment above is retained as an experiment. The source-level repair is
on a separate local branch/worktree, **not** on the transaction-entry patch
branch and not pushed:

- worktree `/home/ava/Worktrees/psycopg/partial-send`
- branch `codex/partial-send-cancel`, based on `origin/master` `99356800`
- commit `fbcbf651` (local only)

### Mechanism

`AsyncConnection.wait()` already had a cancel-and-drain path for an ACTIVE
connection. The new precondition is checked at the cancellation point:

```python
except _INTERRUPTED:
    if self.pgconn.transaction_status == ACTIVE:
        if self.pgconn.flush() != 0:
            # stage tracking: do not resume generator if suspended during send
            if tracker.last_wait is None or (tracker.last_wait & waiting.WAIT_W):
                self.pgconn.finish()
                raise
            # fully sent (fetch stage): keep the existing cancel + drain
```

### Correction: Interleaving window and stage tracking

The initial patch attempt checked `self.pgconn.flush() != 0`. That check was
flawed: **`PQflush()` actively pushes data to the wire; it is not a read-only
stage inquiry**. If backpressure clears right before or during cancellation
handling (or during an interleaving window), `PQflush()` flushes the remaining
buffer and returns 0, but the C generator remains suspended at the send stage's
write readiness yield (`ready = yield WAIT_RW`). Draining subsequently calls
`next(gen)` / `wait_async(gen)`, which passes `None` into the C generator,
triggering `TypeError: an integer is required` and leaving the connection
unusable and stuck in `ACTIVE`. On the pure-Python backend, resuming the send
completes and executes the cancelled statement on the server.

The claim that "PQflush returning 0 proves generator finished send phase" is
therefore **retracted**.

The final upstream fix replaces the `PQflush()` check with direct stage
tracking via `_WaitGenTracker(gen)` wrapping the generator inside
`AsyncConnection.wait()`.
- If `tracker.last_wait is None` or `tracker.last_wait & waiting.WAIT_W`, the
  generator was suspended in the send phase (or before reaching read readiness).
  It immediately closes the connection (`self.pgconn.finish()`) and re-raises
  `_INTERRUPTED`. It does not rely on active flushing or exception string matching.
- If `tracker.last_wait == waiting.WAIT_R`, the generator has completed sending
  and is waiting for server results. Only in this case does the recovery invoke
  `_try_cancel()` and drain via `wait_async()`.
- A second interruption during recovery also fail-closes and preserves cancellation.

### Server-side cleanup & observer verification

The prior statement that "local closed proves backend and locks cleaned up" is
also **retracted**: `conn.closed` is merely a local client flag (`pgconn.status == BAD`)
and does not prove server backend processes or table locks have been released.

`min_repro.py` was revised to perform independent server-side verification:
1. Records the tested connection's `backend_pid` before the partial send.
2. Uses an independent direct connection (bypassing the proxy) as observer.
3. Does not call `conn.close()` to rescue driver omissions; driver must self-close.
4. Separately records client close duration, proxy upstream connection close timing,
   and server backend disappearance. The proxy is kept running during observation
   to distinguish driver vs proxy delays.
5. Bounded wait (5 s timeout) for `pg_stat_activity` and `pg_locks` to drop to 0.
6. Independently acquires an `ACCESS EXCLUSIVE NOWAIT` table lock and confirms 0
   target rows were inserted.
7. Automated negative controls (`--negative-control driver-unclosed`,
   `--negative-control wrong-exception`, `--negative-control unconfirmed-cleanup`)
   verify that validator failures are fatal (exit code 1) and not masked by `finally`.

Empirical timing from PostgreSQL 18.4 (32919):
- Client closed: `True` (0.0000 s)
- Proxy upstream closed: `True` (+0.0021 s after client close)
- Server backend gone: `True` (+0.0021 s after upstream close; 0 in `pg_stat_activity`, 0 in `pg_locks`)
- Independent lock check: `ACCESS EXCLUSIVE` acquired; target row count 0.

### Regression tests

Targeted regression tests in `tests/test_concurrency_async.py` (real server, upstream style):

1. `test_cancel_while_sending_closes_connection` — 8 MB INSERT, 64 KiB proxy
   budget under continuous backpressure; asserts not done, forwarded < payload,
   ACTIVE, `CancelledError`, connection closed, successor `OperationalError: ... closed`,
   observer count 0.
2. `test_cancel_interleaved_proxy_release_only_closes` (Scenario A) — only resumes
   proxy reading without test flush; verifies driver detects send stage and fails closed.
3. `test_cancel_interleaved_active_flush_closes_safely` (Scenario B) — actively drains
   buffer, explicitly asserts prerequisite `flushed_zero` is True, verifies safe fail-closed
   termination without TypeError or ACTIVE hang.
4. `test_cancel_after_send_keeps_connection` — small query fully sent (`WAIT_R`); cancel
   drains, connection remains usable, successor returns 1.
5. `test_second_cancel_during_recovery_closes` — recovery interruption closes
   the wire and still propagates `CancelledError`.
6. `test_large_query_not_cancelled` — normal 8 MB request completes.
7. `test_large_query_ordinary_error_keeps_connection` — a normal server error
   on a large request leaves the connection usable.

### Before/after

Binary C implementation, PostgreSQL 18.4 at 32919:

| build | continuous backpressure cancel | interleaved relief cancel (Scenario B) | connection after | successor | observer target rows |
|---|---|---|---|---|---|
| upstream `99356800` | `TypeError: an integer is required` | `TypeError: an integer is required` | ACTIVE, busy, not closed | `another command is already in progress` | 0 rows |
| fixed with tracker | `CancelledError` (0.001 s) | `CancelledError` (0.001 s) | closed (`tx=4`, not busy) | `OperationalError: the connection is closed` | 0 rows |

Pure-Python implementation (`PSYCOPG_IMPL=python`), same 8 MB INSERT:

| build | continuous backpressure cancel | interleaved relief cancel (Scenario B) | connection after | observer target rows | successor |
|---|---|---|---|---|---|
| upstream `99356800` | `CancelledError` (silently resumed) | `CancelledError` (silently resumed) | IDLE, reusable | **1 (cancelled INSERT committed)** | `(1,)` |
| fixed with tracker | `CancelledError` | `CancelledError` | closed | **0** | `OperationalError: the connection is closed` |

Test commands:

```sh
cd /home/ava/Worktrees/psycopg/partial-send
PYTHONPATH=$PWD/psycopg PSYCOPG_TEST_DSN='host=127.0.0.1 port=32919 user=postgres password=dw dbname=dw' \
  /home/ava/Worktrees/psycopg/tx-cancel/.venv/bin/python -m pytest \
  tests/test_concurrency_async.py -k 'partial or interleaved or cancel_after_send or large_query or second_cancel' -v
```

Seven targeted tests pass on both backends (7/7 C binary, 7/7 pure Python).
The full concurrency async suite passed 20/20 on both backends.
Related async suites passed 574 cursor, 91 pipeline/waiting, 84 transaction tests.
Pure Python backend discovers bundled libpq via `tests/conftest.py` hook without modifying
production package code `pq/misc.py`.

### Scope limits and trade-offs

- The fix covers async `AsyncConnection.wait()` only. The sync `Connection.wait()`
  path and pipeline-mode partial sends are not covered by these tests.
- It is fail-closed: the connection lease is lost; callers must acquire a fresh connection.
  Attempting to recover a reusable connection midway through an unsent message is impossible
  without protocol-level message cancellation, which PostgreSQL wire protocol does not support.
- Outer transaction impact: If the connection held an active outer transaction, closing the
  connection rolls back the transaction on the server. Callers must not expect an outer
  transaction to survive a partial-send cancellation.
- Remote effects: Cancellation does not guarantee undoing operations whose bytes already reached
  and were executed by the server prior to cancellation arrival. For fully sent queries (`WAIT_R`),
  `_try_cancel()` requests cancellation from the server; for partially sent queries (`WAIT_W`),
  dropping the socket guarantees the server never receives a valid complete command.
