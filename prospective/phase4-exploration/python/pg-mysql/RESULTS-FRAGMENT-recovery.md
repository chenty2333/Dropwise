# RESULTS-FRAGMENT recovery — mysql-connector-python 26.7.0 async direct: recovery calls and a minimal cancel-poison patch

Status: **exploratory round-4 (2026-10-22), not Phase 4 data, unfrozen.**
This fragment closes the two gaps left by `RESULTS-FRAGMENT-round3b.md` §5 for
`mysql-connector-python 26.7.0` async direct: (a) a **recovery contrast** after
cancelling an in-flight command, and (b) a **minimal fix contrast** against a
patched copy of the installed connector. No existing file, log, container or
`research/python-retro/` file was modified; no commit, push, issue or mail.

Raw logs (new directories, old logs untouched):

| run | purpose | path |
|---|---|---|
| recovery matrix, original build | 4 cuts × 3 strategies × 3 reps | `logs/run.zp7wH6/` |
| fix contrast, final | original vs patched on one server | `logs/run.1fMJtP/` |
| pre-guard patched build | intermediate (see §4.1 and §10) | `logs/run.gTlHvk/` |

New code: `cases/mysqlconnector_recovery_case.py`,
`run-mysqlconnector-recovery.sh`, `run-mysqlconnector-fix-contrast.sh`,
`fix/patched/mysql/connector/**` (+ `fix/patched/PATCH-NOTES.md`).

## 1. Versions, image, environment

| item | value |
|---|---|
| Python | CPython **3.13.13** (`~/dropwise-py-work-r3/venv313`, uv-managed) |
| mysql-connector-python | **26.7.0** (site-packages, pure-Python `mysql.connector.aio`) |
| MySQL image | `mysql:9.6`, digest `sha256:c5df04bee1a42b74a5841c6409e669cf62126cd0416f00c1cea8ab933b9361b9` |
| Server | `9.6.0` |
| Proxy | `DelayProxy` from `research/python-retro/cases/common.py`, **50 ms per server→client chunk**, tested handle only |
| Observer | separate direct `mysql.connector.aio` connection, no proxy, never the tested handle |
| Bound | every operation 2.0 s (`bound()`), target 12 s, setup 15 s |
| General log | `log_output=TABLE`, `general_log=ON` per scenario, queried from the observer, filtered by tested connection id; `event_time` is microsecond-resolution |

Both runners start a disposable `dw-r4-mcrec-$$` / `dw-r4-mcfix-$$` container
with `-p 127.0.0.1::3306` and remove it on exit. The fix contrast runs original
and patched **sequentially on the same container instance**; the recovery matrix
and the patched recovery matrix ran on two different container instances of the
same image (noted in §10).

## 2. Exact commands

Recovery matrix (original build, new container `dw-r4-mcrec-676567`):

```sh
cd prospective/phase4-exploration/python/pg-mysql
./run-mysqlconnector-recovery.sh
# inside, for k in 1..4 and s in rollback reset_session close_reconnect:
PYTHONPATH=".:$REPO/research/python-retro" \
  ~/dropwise-py-work-r3/venv313/bin/python -m cases.mysqlconnector_recovery_case \
  --port $PORT --user root --password dw --dbname $DB --cut $k --strategy $s --reps 3
# plus (no service): python tests/test_adapter_contract.py
```

Fix contrast (new container `dw-r4-mcfix-696697`, sequential):

```sh
./run-mysqlconnector-fix-contrast.sh
# original
PYTHONPATH=".:$REPO/research/python-retro" python -m cases.mysqlconnector_case \
  --port $PORT --user root --password dw --dbname $DB --mode direct --diag
PYTHONPATH=".:$REPO/research/python-retro" python -m cases.mysqlconnector_case \
  ... --probe 1,2,3,4            # ×3, fresh process per rep
PYTHONPATH=".:$REPO/research/python-retro" python -m cases.mysqlconnector_case \
  ... --levels B1,B2,B3,M1 --reps 3
# patched (same commands, patched copy first)
PYTHONPATH="$PWD/fix/patched:.:$REPO/research/python-retro" ...
# patched recovery matrix: 4 cuts × 3 strategies × 3 reps, same module as §2
```

The patched tree is loaded via `PYTHONPATH`; the run log
`logs/run.1fMJtP/patched-import.txt` records:

```
mysql.connector.__file__ = .../python/pg-mysql/fix/patched/mysql/connector/__init__.py
aio.network.__file__     = .../fix/patched/mysql/connector/aio/network.py
aio.abstracts.__file__   = .../fix/patched/mysql/connector/aio/abstracts.py
PATCH ACTIVE
```

## 3. Recovery matrix (original build) — what the cancelled cut actually left on the wire

36/36 target runs were realised (`cancelled`, yields 1/2/3/4); every cell below
was identical in 3/3 reps (except cids/table names). The "wire state at cancel"
column is read from the general log, not inferred: the log entry proves the
statement reached the server; the client never read its reply.

Legend: `F/F/T` = `in_transaction` / `unread_result` / `is_socket_connected`.

| cut | wire state at cancel (general log) | after_cancel | target |
|---|---|---|---|
| 1 | `START TRANSACTION` reached server; reply unread | F/F/T | cancelled y1 |
| 2 | internal `SELECT @@session.sql_mode` reached server; reply unread; target `REPLACE` **not** sent | T/F/T | cancelled y2 |
| 3 | target `REPLACE(target,1)` reached server; reply unread; no COMMIT | T/F/T | cancelled y3 |
| 4 | `REPLACE(target,1)` returned; target `COMMIT` reached server; reply unread | T/F/T | cancelled y4 |

Observer sample *before* recovery (independent connection):
cut 1: table empty, `innodb_trx` 0, `performance_schema.events_transactions_current`
has no row for the tested cid (a `START TRANSACTION`-only transaction is not
listed), general log above is the evidence;
cuts 2–3: `STATE=ACTIVE` for the tested cid, `innodb_trx` empty (cut 2 has no
InnoDB statement yet; cut 3's `REPLACE` had not yet produced a listed trx);
cut 4: `innodb_trx` one `RUNNING` row for the tested cid + `STATE=ACTIVE`.

### 3.1 `rollback()` — 12/12 calls returned OK, 0/12 recovered the handle

| cut | call | after_recovery | survivor `REPLACE` / same-handle `SELECT` | observer rows | general log order (tested cid only) |
|---|---|---|---|---|---|
| 1 | `ok` | T/F/T | `ProgrammingError: Failed processing format-parameters; 'NoneType' object is not subscriptable` / **`ok [["ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,..."]]`** | `[]` | `START TRANSACTION`, `ROLLBACK`, `SELECT @@session.sql_mode` ×2, `SELECT v ...` |
| 2 | `ok` | T/T/T | `InternalError: Unread result found` / same | `[]` | `START TRANSACTION`, `SELECT @@session.sql_mode`, `ROLLBACK` |
| 3 | `ok` | T/F/T | `ok 0` / `ok []` | `[["survivor",7]]` | `... REPLACE target ...`, `ROLLBACK`, `REPLACE survivor`, `SELECT v ...` |
| 4 | `ok` | F/F/T | `ok 0` / `ok []` | `[["survivor",7],["target",1]]` | `... REPLACE target ...`, `COMMIT`, `ROLLBACK`, `REPLACE survivor`, `SELECT v ...` |

Concrete reading:

* cut 1: `rollback()` consumed the unread `START TRANSACTION` OK as its own
  result and returned OK.  The stream stayed one reply behind.  The survivor
  `REPLACE` was never sent (the client failed while escaping parameters); the
  next `SELECT` returned the connector's internal `sql_mode` row as **user
  data**, with no error.
* cut 2: the stale reply was a result-set header.  `rollback()` parsed it as a
  result set (`unread_result=True`) and still returned OK.  The server executed
  `ROLLBACK` (`STATE=ROLLED BACK`), but the handle is now unusable — every
  later command raises `InternalError: Unread result found`.
* cut 3: the server executed `ROLLBACK` and then the survivor `REPLACE`, which
  **committed** (observer sees `survivor=7`).  The client reports `rowcount 0`
  (the write read `ROLLBACK`'s OK) and an **empty readback** (the following
  `SELECT` read the survivor `REPLACE`'s OK, so `fetchall()` had no rows).
* cut 4: the target `COMMIT` had already executed, so the target row is
  committed; the same one-reply shift then mis-reports the survivor write as
  `rowcount 0` / empty readback while the observer sees both rows.

Server transaction after the call: closed in all four cuts (`COMMITTED` or
`ROLLED BACK`); `rollback()` did reach the server and did close the transaction.
The failure is client-side reply association, not a server-side open
transaction.

### 3.2 `reset_session()` — 9/12 calls returned OK, 0/12 recovered the handle

`reset_session()` is the public API; it calls `is_connected()` (`cmd_ping`),
then `cmd_reset_connection()`, then `_post_connection()`.  The general log does
not record COM_PING / COM_RESET_CONNECTION, but the post-reset
`SET NAMES` / `SET @@session.autocommit` / `SET @@session.sql_mode` entries are
only sent by `_post_connection()` after a successful reset.

| cut | call | after_recovery | survivor `REPLACE` / `SELECT` | observer rows | general log order | server tx at observer |
|---|---|---|---|---|---|---|
| 1 | `ok` | F/F/T | `ProgrammingError ... 'NoneType' object is not subscriptable` / `sql_mode` string as row | `[]` | `START TRANSACTION`, `SET NAMES`, `SET autocommit`, `SELECT @@session.sql_mode` ×2, `SELECT v` | closed |
| 2 | **`err OperationalError: MySQL Connection not available.`** | T/F/T | `ProgrammingError ... bytearray index out of range` / same | `[]` | `START TRANSACTION`, `SELECT @@session.sql_mode` — **no reset, no ROLLBACK, no survivor write** | **`ACTIVE`** |
| 3 | `ok` | F/F/T | `ok 0` / `ok []` | `[["survivor",7]]` | `... REPLACE target ...`, `SET NAMES`, `SET autocommit`, `SET sql_mode`, `REPLACE survivor`, `SELECT v` | closed |
| 4 | `ok` | F/F/T | `ok 0` / `ok []` | `[["survivor",7],["target",1]]` | `... REPLACE target ...`, `COMMIT`, reset SETs, `REPLACE survivor`, `SELECT v` | closed |

* cut 1: same false success as `rollback()`; the reset did happen server-side
  (post-reset SETs, transaction closed), the client stayed shifted, and the
  `SELECT` returned the internal `sql_mode` row as user data.
* cut 2 is the strongest **failed-recovery** case: `is_connected()`'s `PING`
  read the stale result-set header, `_handle_ok` raised `InterfaceError`, and
  `is_connected()` swallowed it into `False`.  `reset_session()` therefore
  raised `OperationalError: MySQL Connection not available.` *without sending
  the reset*.  The server transaction stayed `ACTIVE`, the survivor `REPLACE`
  never reached the server, and the client reported a parameter-escaping error.
  Only `close()`/disconnect cleaned it up.
* cuts 3–4: reset succeeded server-side, and the shift again made a committed
  survivor write look like `rowcount 0` + empty readback.

### 3.3 `close()` + reconnect — 12/12 true recovery

| cut | close | reconnect | after_recovery | survivor `REPLACE` / `SELECT` | observer rows | server tx |
|---|---|---|---|---|---|---|
| 1 | `ok` | `ok`, new cid | F/F/T | `ok 1` / `ok [[7]]` | `[["survivor",7]]` | closed |
| 2 | `ok` | `ok`, new cid | F/F/T | `ok 1` / `ok [[7]]` | `[["survivor",7]]` | closed |
| 3 | `ok` | `ok`, new cid | F/F/T | `ok 1` / `ok [[7]]` | `[["survivor",7]]` | closed |
| 4 | `ok` | `ok`, new cid | F/F/T | `ok 1` / `ok [[7]]` | `[["survivor",7],["target",1]]` | closed |

`close()` does not read any reply (it sends QUIT only if the socket still
reports connected, then closes the socket), so it is immune to the unread
reply; the server rolls the old transaction back on disconnect; a fresh
connection has a clean stream.  This is the only strategy that recovered in all
12 runs.  The general log shows the new connection's handshake plus
`SET NAMES`/`SET autocommit`/lazy `SELECT @@session.sql_mode`, then the
survivor `REPLACE` and `SELECT`.

### 3.4 Aggregate (original build, 36 executions)

| outcome after the cancellation | rollback | reset_session | close+reconnect |
|---|---|---|---|
| recovery call returned OK | 12/12 | 9/12 | 12/12 |
| handle actually recovered | **0/12** | **0/12** | **12/12** |
| OK call + later command silently returned wrong values | 9/12 (cut 1, 3, 4) | 9/12 (cut 1, 3, 4) | 0/12 |
| OK call + later command failed loudly (`Unread result`) | 3/12 (cut 2) | 0/12 | 0/12 |
| recovery call error + server transaction left open | 0/12 | 3/12 (cut 2) | 0/12 |

21/36 recovery executions (58%) and **21/24 rollback/reset executions (87.5%)**
returned OK from an official recovery call while the handle was still not
recovered.  18/36 returned wrong user-visible values with no error at the
recovery call.

Execution count across both runs: 36 original recovery + 36 patched recovery +
24 probe + 96 ladder trials + 2 diag = **194 logged executions**, plus 16
uncancelled calibration scenarios (B1/B2/B3 one `count_yields` each, M1 five
per build, two builds) = **210 scenario instantiations**, all consistent with
**one independent root cause** (§5): a cancelled in-flight command leaves the
stream shifted and the connection reusable.

## 4. Fix contrast — cancel closes/poisons instead of leaving the handle reusable

### 4.1 Patch

Copied `mysql/connector` from the venv into `fix/patched/mysql/connector`
(plus `mysql/__init__.py` so the package shadows site-packages; the unrelated
`mysql/vendor` and `mysql/ai` trees were omitted), then two minimal edits
(full detail in `fix/patched/PATCH-NOTES.md`):

* `aio/network.py`: `MySQLSocket` gains `_close_reason` and
  `_close_on_cancel()` (close + `transport.abort()` + reason).  `read()` and
  `write()` catch `asyncio.CancelledError`, call it and **re-raise the same
  CancelledError** (never convert it).
* `aio/abstracts.py` (`handle_unread_result`): a cancel-poisoned socket raises
  `InterfaceError("Cancelled during execution; connection is not reusable")`
  on any later command — except while the task is unwinding a cancellation
  (`task.cancelling()`), because cursor/context cleanup calls this method and
  raising there would replace the in-flight `CancelledError` with
  `InterfaceError`.  That guard was added after the pre-guard run
  `logs/run.gTlHvk` showed patched B1 cut 2 as 3/12 violations with
  `ProgrammingError: Failed processing format-parameters; Cancelled during
  execution;...`; with the guard patched B1 is 0/12.

Shape follows aiomysql: `aiomysql/connection.py:602-613` catches
`CancelledError` in `_read_packet` and calls `_close_on_cancel()`
(`:1116-1118`: `self.close(); self._close_reason = "Cancelled during
execution"`), and `_ensure_alive()` (`:1120-1126`) raises
`InterfaceError(self._close_reason)` afterwards.  The mysql-connector patch
puts the same decision in the socket layer so it covers `_send_cmd`,
`_handle_result`, `get_rows` and cursor reads.

### 4.2 Normal path (regression)

Patched `--diag` (`logs/run.1fMJtP/patched-diag.txt`): cid stays `150` across
the three reuse probes, `REPLACE`/`SELECT` return `ok 1`/`[[7]]`, the
uncancelled target completes (`start_transaction`/`insert`/`commit`, in_tx
False after), and the pre/post observations are the normal ones (both contain
`survivor=7`; post also contains `target=1`).  Identical shape to the original
build's diag.  The patch does not change non-cancelled behaviour.

### 4.3 Four-cut probe, original vs patched (3 reps each; 3/3 identical within a cell)

| cut | original survivor (`write` / `own`), observer | patched survivor (`write` / `own`), observer |
|---|---|---|
| 1 | `err ProgrammingError ... 'NoneType' ...` / `ok [[sql_mode string]]`; `[]` | `err OperationalError: MySQL Connection not available` / same err; `[]` |
| 2 | `ok 1` / `ok []`; `[]` | `err OperationalError ...` / same err; `[]` |
| 3 | `ok 1` / `ok []`; `[]` | `err OperationalError ...` / same err; `[]` |
| 4 | `ok 0` / `ok []`; `[survivor,7],[target,1]` | `err OperationalError ...` / same err; `[]` |

All 12 patched target runs were still `cancelled` (realized, yields 1–4): the
patch preserves the cancellation.  `in_tx` is a local flag and still shows the
pre-cancel value (`False` for cut 1, `True` for cuts 2–4).

The patched cut-4 observer `[]` needs care: the probe does not enable the
general log, and the patched recovery matrix shows both sub-states for cut 4 —
when the `COMMIT` is logged (close+reconnect runs) the target row is present
(`[survivor,target]`); when it is not logged (rollback/reset runs) it is absent.
So the probe's `[]` is a realisation race (the abort can discard a COMMIT that
the server has not read yet), not the patch rewriting server state.  The
original build always logged `COMMIT` and always committed the target row.

### 4.4 Ladder, original vs patched (3 reps per cut; 4 cuts)

| level | original | patched | note |
|---|---|---|---|
| B1 | 0/12 violations | 0/12 violations | cancel-only; `CancelledError` preserved by the patch |
| B2 | 12/12 violations | 12/12 violations | original: cut 1 `ProgrammingError ... 'NoneType'`; cuts 2–4 `ProgrammingError: Transaction already in progress` (documented fail-fast). patched: cut 1 `InterfaceError: Cancelled during execution; connection is not reusable`; cuts 2–4 unchanged fail-fast |
| B3 | 12/12 violations | 12/12 violations | same shapes as B2 |
| M1 | 12/12 `Neither` | 12/12 `Neither` | original survivor values are stale/desynced; patched survivor is a loud `OperationalError` |

`unknown=0`, `unrealized=0` everywhere.  **The patch does not reduce the ladder
counts**: M1 still sees `Neither` because the survivor cannot run on the
invalidated handle; B2/B3 still count fail-fast as violations.  What changes is
the *content*: no stale success, no wrong data, no internal `sql_mode` row as
user data — the next use fails loudly.

### 4.5 Patched recovery matrix (4 cuts × 3 strategies × 3 reps)

| strategy | recovery call | after_recovery | survivor | server transaction at observer sample |
|---|---|---|---|---|
| `rollback()` | `InterfaceError: Cancelled during execution; connection is not reusable` (12/12) | socket_connected False | `err OperationalError: MySQL Connection not available` (12/12) | cut 1 `COMMITTED` (no open tx shown); cuts 2–4 `ACTIVE` (cut 4 also `innodb_trx RUNNING`) — the handle cannot roll back; close+reconnect closes it, teardown timing otherwise not measured |
| `reset_session()` | `OperationalError: MySQL Connection not available.` (12/12; `is_connected()` maps the poison to "not available") | socket_connected False | `err OperationalError: MySQL Connection not available` (12/12) | same as rollback |
| `close()`+reconnect | close `ok`, reconnect `ok` (12/12) | new cid, clean | `ok 1` / `ok [[7]]` (12/12) | closed; observer `[survivor]` (cuts 1–3) or `[survivor,target]` (cut 4) |

So the patch **removes the silent false recovery** (no official call can consume
a stale reply any more), but it does not make `rollback()`/`reset_session()` work
on the poisoned handle: they fail loudly and the caller must close/reconnect;
the server-side transaction can stay open until that happens.  This is the
aiomysql shape (connection invalidated, not repaired).

## 5. Source mechanism (original 26.7.0, file/line)

* `mysql/connector/aio/connection.py:607-643` — `_send_cmd()` sends the command
  and awaits `self._socket.read()` at **line 643**; there is no
  `except CancelledError` and no close/poison.  The
  `@handle_read_write_timeout()` decorator (`aio/_decorating.py:90-107`) only
  closes on `ReadTimeoutError`/`WriteTimeoutError`, not on cancellation.
* `aio/connection.py:500-547` — `_handle_result()` sets `self.unread_result =
  True` at **line 547** only after the packet was successfully read.  A cancel
  during the read leaves `unread_result == False`, the socket open and the
  reply bytes in the stream (`after_cancel` is F/F/T in every cut).
* `aio/abstracts.py:1505-1516` — `handle_unread_result()` only looks at
  `unread_result`; it has no notion of a pending/unread *reply* and no socket
  state check.
* `aio/connection.py:992-996` — `rollback()`: `if self.unread_result:
  await self.get_rows()` then `_execute_query("ROLLBACK")`.  With
  `unread_result == False` it skips the drain, sends ROLLBACK and lets
  `cmd_query` parse the stale reply; the return value of `_handle_result` is
  ignored, so the call returns OK.  If the stale reply is a result-set header,
  `_handle_result` consumes the header and leaves `unread_result=True` — a
  later command is then loud (`InternalError: Unread result found`).
* `aio/connection.py:708-758` — `reset_session()`: `is_connected()` →
  `cmd_ping()` (`_handle_ok(_send_cmd(PING))`, `:1492`) consumes one stale
  packet; if it is an OK packet, `cmd_reset_connection()`
  (`:998-1010`) consumes the PING reply as the reset reply and returns True;
  `_post_connection()` (`aio/abstracts.py:1199-1215`) sends SET NAMES /
  autocommit / sql_mode, each consuming the previous reply.  The function then
  returns OK with one reply still pending.  If the stale packet is a result-set
  header, `_handle_ok` raises `InterfaceError`, `is_connected()` catches it as
  "False", and `reset_session()` raises `OperationalError` **before sending
  RESET_CONNECTION** — the server transaction stays open.
* `aio/cursor.py:175-216` — parameter substitution calls `get_sql_mode()`
  (`_process_params_dict()` line 181, `_process_params()` line 204); that
  method (`aio/abstracts.py:1015-1019`) issues the internal
  `SELECT @@session.sql_mode` via `info_query`.  In cut 1 the head of the
  stream is an OK packet (the `START TRANSACTION` reply, or after a `rollback()`
  the ROLLBACK reply), so the internal query consumes it, `fetchone()` returns
  None and the substitution fails; the user's own parameterised `SELECT` then
  reads the internal sql_mode result set and returns the sql_mode string as
  user data.  At cut 3/4 the sql_mode is already cached, so the shift shows up
  directly on the survivor `REPLACE`/`SELECT`.
* `aio/connection.py:799-810` — `close()` suppresses `Error` around cursor
  closes, sends QUIT only if the socket still reports connected, then closes
  the socket and sets `_socket = None`; it never reads a reply, which is why
  close+reconnect recovers.
* `aio/network.py:641-643` (socket `read`) and `aio/network.py:568-581`
  (`close_connection`) have no cancellation handling either.

Patched locations: `fix/patched/mysql/connector/aio/network.py:560` (flag),
`:594-611` (`_close_on_cancel`), `:651-680` (write/read catch), and
`fix/patched/mysql/connector/aio/abstracts.py:1513-1522` (refuse reuse).
aiomysql reference: `aiomysql/connection.py:602-613, 1116-1126`.

## 6. Classification

### 6.1 Verdict

**Confirmed client bug, with fix contrast.**  The connector leaves the
connection reusable after a cancelled in-flight command while the reply stream
is shifted; `unread_result` remains False, so no layer notices.  Official
recovery calls can return OK on this state and then either silently
mis-associate replies (18/36 executions returned wrong user-visible values) or
fail loudly later (3/36), and one recovery call
(`reset_session()` after cut 2) reports an error and leaves the server
transaction open (3/36).  This is not "contract-external": the docstrings of
`rollback()` ("Rollback current transaction") and `reset_session()` (raises
only for not-connected / unread / interface errors) do not describe a
successful call that leaves the handle misaligned, and the async documentation
does not define cancellation semantics for an in-flight command.  The
`start_transaction()` fail-fast (`ProgrammingError: Transaction already in
progress`, docstring `aio/abstracts.py:1694-1697`) is documented and is not part
of the bug.

### 6.2 Upstream known-vs-new (search only; no tracker triage)

A web search on 2026-10-22 (queries around `mysql-connector-python asyncio
cancel CancelledError unread result` and `mysql.connector.aio cancellation
desync`) returned generic connector/async docs, StackOverflow "Unread result
found" posts, an aiomysql cancellation issue and a gunicorn `CancelledError`
issue — **no matching mysql-connector-python report**.  The MySQL manual pages
(`dev.mysql.com`, `docs.oracle.com`, mirror `dev.mysqld.com.cn`) could not be
fetched from this environment: `web_fetch` failed with "hostname resolves to a
non-public IP address".  Therefore: **not found in a limited search; no
upstream issue-body triage was performed; do not read this as "new" or
"known"**.  The installed package's own docstrings
(`aio/abstracts.py:1671-1705`, `aio/connection.py:708-757, 992-996`) and source
were read directly.

## 7. No-service and patch-active checks

* `tests/test_adapter_contract.py` (fake `Scenario`, no DB/proxy):
  `ALL ADAPTER-CONTRACT TESTS PASSED` — five checks, `logs/run.zp7wH6/adapter-contract-tests.txt`.
* Patched copy is actually loaded: `logs/run.1fMJtP/patched-import.txt` asserts
  `/fix/patched/` in `mysql.connector.__file__`, prints the three module paths
  and `PATCH ACTIVE`.
* Both runners exited 0; every per-command log file ends with `exit=0`
  (the environment/command-note files do not carry an exit status).

## 8. Limits

* One server image/version (`mysql:9.6`, 9.6.0), no MariaDB, no TLS, no
  compression, no connector pool mode, no multi-handle/concurrent use.
* One handle per scenario, one cancellation depth (4 realised yields), 3 reps;
  the target is a fixed `start_transaction`/`REPLACE`/`commit` shape.
* The recovery matrix (original) and the patched recovery matrix ran on two
  different containers of the same image; the original-vs-patched probe/ladder
  ran on one container sequentially.
* General log is `TABLE` output and adds a write per statement; it changes
  timing (though the cuts are yield-based).  `COM_PING` and
  `COM_RESET_CONNECTION` are not visible in the general log, so `reset_session`
  reaching the server is inferred from its post-connection SETs and from the
  row/transaction state, not from a RESET log line.
* `performance_schema.events_transactions_current` does not list a
  `START TRANSACTION`-only transaction (cut 1); `innodb_trx` only lists
  transactions that have done InnoDB work and had not yet done so at cut 3's
  observer sample.  The general log is the primary wire evidence.
* Cut 4 realisation is not identical between runs: the cancel can land in the
  COMMIT write or in the COMMIT read, so whether the COMMIT reached the server
  differs (patched rollback/reset cut 4: no COMMIT in the log, target row
  absent; patched close+reconnect cut 4: COMMIT logged, target row present).
  This is a property of cancelling an in-flight COMMIT, not a controlled
  variable.
* The patched server-side transaction stayed `ACTIVE` at the observer sample
  for cuts 2–4 after the poisoned handle's failed recovery; teardown timing
  (proxy propagating the aborted socket, server rolling back on disconnect) was
  not measured.  The `close()`+reconnect path demonstrably closes it.
* `Task.cancelling()` requires Python 3.11+; the guard leaves a narrow reuse
  hole if a caller catches `CancelledError` and reuses the connection before
  `uncancel()` (not exercised).
* No upstream issue/tracker triage; docs fetch blocked (see §6.2).
* The existing `cases/mysqlconnector_case.py` still constructs target
  coroutines eagerly, so its logs contain `RuntimeWarning: coroutine ... was
  never awaited` for cancelled stages.  It is pre-existing code and was not
  modified; the new recovery case builds stages lazily.

## 9. Facts vs interpretation

**Facts (from the logs):**

1. 36/36 original recovery targets were cancelled at the intended yield; every
   cut left `in_transaction`/`unread_result`/socket exactly as in §3.
2. `rollback()` returned OK in 12/12, `reset_session()` in 9/12,
   `close()`+reconnect in 12/12.
3. After 21 of those 24 OK rollback/reset calls the same handle did not behave
   like a recovered connection: 18 runs returned wrong user-visible values
   (sql_mode string as a row, or `rowcount 0` + empty readback for a write the
   observer saw committed), 3 runs raised `InternalError: Unread result found`.
4. `reset_session()` after cut 2 raised `OperationalError` without sending the
   reset; the independent observer still saw `STATE=ACTIVE` for that
   connection and the general log had no ROLLBACK/COMMIT.
5. `close()`+reconnect gave a new connection id, correct survivor write and
   readback, and no open transaction in 12/12.
6. The patched build preserved `CancelledError` in 12/12 B1 runs, made every
   later command on the cancelled handle fail loudly, and still allowed
   normal (uncancelled) use and close+reconnect recovery.
7. Patched M1/B2/B3 counts are unchanged (12/12 violations/Neither); the
   stale/desynced values become `InterfaceError`/`OperationalError`, except
   that B2/B3 cuts 2–4 still show the documented
   `ProgrammingError: Transaction already in progress`.

**Interpretation (not proven by the logs):**

* The reply-stream shift is the single root cause of all observed shapes;
  the code path in §5 is consistent with every value, but no packet-level
  capture was taken (the general log shows statements, not reply bytes).
* The cut-4 difference between patched probe `[]` (3/3 reps) and patched
  close+reconnect `[target]` (3/3 reps) is a commit/abort race, not a semantic
  choice of the patch.
* The patch is a safety fix, not a correctness fix for the cancelled
  operation: it prevents the *next* command from consuming a stale reply, but
  the cancelled command's outcome (and an in-flight COMMIT's fate) remains
  unknown to the caller.
* "False recovery" is used operationally: an official recovery call returned
  OK while a later command on the same handle did not reflect the server state.
  No claim is made that `rollback()`/`reset_session()` failed to execute on the
  server when they returned OK — the general log shows they did execute (except
  the cut-2 reset error, where RESET was not sent).

## 10. Uncovered / next

* No MariaDB, TLS, compression, pool mode, prepared statements, multi-row
  result sets, cancellation during `executemany`, or concurrent use of the same
  handle.
* No direct `cmd_reset_connection()` strategy (only `reset_session()`, its
  public wrapper); no `ping(reconnect=True)` recovery.
* No packet capture and no client-side stack at the moment of cancellation
  beyond the yield index.
* No upstream tracker triage or maintainer contact; docs pages unfetchable here.
* No test of the patched build's `rollback()`/`reset_session()` on a handle
  whose cancellation happened while reading a *multi-row* result set (the
  socket-layer patch should cover it; not exercised).
* The pre-guard patched run (`logs/run.gTlHvk`) is kept as an intermediate:
  B1 cut 2 was 3/12 violations there because cursor cleanup replaced the
  `CancelledError`; the guard in the final patch fixed that.  Old logs were not
  overwritten.
