# Upstream reproductions

Submitted on 2026-10-22:

- psycopg_pool waiter cleanup patch: [PR #1452](https://github.com/psycopg/psycopg/pull/1452).
- psycopg transaction entry cancellation: [issue #1451](https://github.com/psycopg/psycopg/issues/1451), `psycopg_entry.py`.
- asyncpg transaction entry cancellation: [reproduction added to #1386](https://github.com/MagicStack/asyncpg/issues/1386#issuecomment-6328277427), `asyncpg_entry.py`.
- MySQL async query reply mismatch: [issue #112](https://github.com/mysql/mysql-connector-python/issues/112), `mysql_cancel.py`.

The scripts use only the corresponding driver and the standard library. Run
against disposable local databases. The PostgreSQL scripts need CREATE TABLE
permission and take `DATABASE_URL`; the MySQL script uses root and takes
`MYSQL_PORT` and `MYSQL_PASSWORD`.

The transaction scripts demonstrate a successful retry whose insert is not
visible from an independent connection. The MySQL script confirms the first
query is running before cancelling it, then demonstrates its reply being
returned for `SELECT 42`.

## Repair PR submission status (2026-10-22)

- psycopg transaction-entry repair: [PR #1454](https://github.com/psycopg/psycopg/pull/1454), including the reviewed active-transaction re-entry correction.
- asyncpg failed-start registration repair: [PR #1399](https://github.com/MagicStack/asyncpg/pull/1399).
- MySQL cancellation repair: still local. The user chose to defer submission rather than continue Oracle registration; no agreement was signed on the user's behalf and no MySQL PR was opened.

The published branches match the locally reviewed commits. Remote checks and
maintainer acceptance are separate from the local results below.

## Local patch worktrees (2026-10-22)

Three independent clean worktrees, each branched from the verified upstream
baseline. At preparation time all changes were local; subsequent submission status is
recorded above. Each branch was tested with its production change reverted to the
baseline (`git show HEAD^:<file>`) to confirm the new tests fail before the fix.

### psycopg #1451

- checkout: `/home/ava/Worktrees/psycopg/tx-cancel`
- branch: `codex/tx-entry-cancel`
- baseline: `origin/master` `99356800` ("fix: don't poll the cancel connection if the socket is not ready")
- patch: `5f295ab1` ("fix: recover async transaction state on cancelled entry")
- files: `psycopg/psycopg/transaction.py`, `tests/test_transaction_async.py`, `docs/news.rst`
- fixed tests: `PSYCOPG_TEST_DSN='host=127.0.0.1 port=32919 user=postgres password=dw dbname=dw' .venv/bin/python -m pytest tests/test_transaction_async.py tests/test_transaction.py -q` -> 177 passed, 9 skipped
- baseline: 6 failed, 171 passed, 9 skipped; the six failures are the new entry-cancellation/savepoint tests, while the lock-wait control and the sync transaction tests pass
- broader async suite (`test_connection_async`, `test_cursor_*_async`, `test_pipeline_async`, `test_tpc_async`, `test_prepared_async`, `test_concurrency_async`, `test_notify_async`): 784 passed, 37 skipped excluding `test_resolve_error_host` (the local resolver maps `nosuchhost.example.com` to a non-public address instead of failing DNS as the test expects)
- checks: `black --check`, `flake8`, `mypy --follow-imports=silent` clean on changed files
- PR draft: `PSYCOPG-PR-1451.md`
- coordinator review found and fixed a re-entry regression in the submitted patch: rejecting a second `__aenter__()` on the same active object must not roll back the original transaction. The new control passes on upstream, fails with the submitted patch, and passes after the local follow-up, with pipeline both off and on.
- remaining coverage: pipeline-entry cancellation is not covered (the command-queue path does not suspend like the direct wire path); full suite and remote CI were not run

### asyncpg #1386

- checkout: `/home/ava/Worktrees/asyncpg/repo`
- branch: `codex/tx-start-cleanup`
- baseline: `13b01910` ("Open 0.33.0 development")
- patch: `ff1bb8d6` ("Fix stale _top_xact after a failed Transaction.start()")
- files: `asyncpg/transaction.py`, `tests/test_transaction.py`
- fixed tests: `PGHOST=127.0.0.1 PGPORT=32919 PGUSER=postgres PGPASSWORD=dw PGDATABASE=dw .venv/bin/python -m unittest -q tests.test_transaction` -> 12 tests, OK
- baseline: same command -> 4 failures (`test_start_failure_does_not_poison_connection`, `test_cancelled_start_does_not_poison_connection`, `test_cancelled_start_refuses_reuse_until_reset`, `test_cancelled_start_pool_release`); the two nested-transaction tests pass on both versions
- additional fixed runs: `tests.test_pool` 66 OK, 4 skipped; `tests.test_cancellation` + `tests.test_timeout` 23 OK; `tests.test__sourcecode` 2 OK, 1 skipped; `flake8` clean
- PR draft: `ASYNCPG-PR-1386.md`; baseline failure log: `asyncpg-baseline-failure-1386.txt`
- remaining coordinator checks: no deterministic server-side `BEGIN` `ErrorResponse` was produced (nested `SAVEPOINT` failure covers the same cleanup path); direct `conn.execute()` after a cancelled `BEGIN` remains a manual-transaction caveat; full suite and remote CI were not run

### mysql-connector-python #112

- checkout: `/home/ava/Worktrees/mysql-connector/aio-cancel`
- branch: `codex/aio-cancel-stream`
- baseline: `origin/trunk` `79f34351` ("Fix mysqld version parsing for multi-digit components")
- patch: `e7a02b6` ("Fix aio connection reuse after a cancelled command")
- files: `lib/mysql/connector/aio/network.py`, `lib/mysql/connector/aio/abstracts.py`, `lib/mysql/connector/aio/cursor.py`, `tests/test_aio_connection.py`
- fixed focused tests: `cd mysql-connector-python && ../.venv/bin/python unittests.py --use-external-server --host 127.0.0.1 --port 32922 --user root --password dw -T tests.test_aio_connection.MySQLAioCancellationTests --verbosity 1` -> 11 tests, OK
- baseline: same command -> 5 failures + 4 errors in 7.7 s; buffered, unbuffered and `nextset()` reuse the cancelled reply as `[(0,)]` / `('cancelled_value',)`, the cancelled fetch blocks for more than 5 s on the stale reply, the prepared case gets `InterfaceError: Expected Binary OK packet`, both cursor-context cases get `InternalError: Unread result found` instead of `CancelledError`, the compressed-write unit test gets `WriteTimeoutError` instead of `CancelledError`, and the socket-poisoning unit test fails on the missing `_is_poisoned` attribute; the two controls pass on both versions
- fixed module runs: `aio_connection` 94 OK, 7 skipped; `aio_cursor` 75 OK; `aio_pooling` 18 OK; `qa_aio_timeout` 22 OK, 5 skipped
- manual compressed check with `compress=True`: cancellation disconnects the socket, the next query raises `OperationalError`, a fresh connection returns `[(42)]` / `('next_value',)`
- checks: `black --check` clean on the four changed files; `isort --check-only` clean on the three production files (`tests/test_aio_connection.py` retains import-order differences already present on trunk); `compileall` passes
- PR draft: `MYSQL-PR-112.md`
- remaining coordinator checks: C-extension path and a dedicated cancelled server-side prepared fetch are not covered; compression is covered by a unit test plus the manual check but not by an integration test; full suite and remote CI were not run

## Coordinator closeout (2026-10-22)

The three production diffs, tests and drafts have been reviewed locally. The
psycopg re-entry regression was corrected in a separate follow-up commit on
`codex/tx-entry-cancel`; the branch now contains both the original repair and
that correction. No production correction was needed for asyncpg or MySQL.

Independent reruns confirmed:

- psycopg: 177 transaction tests passed, 9 skipped; the broader async selection
  passed 784, skipped 37, with the known resolver test excluded. Replacing only
  the production module with upstream reproduced the six entry failures;
  re-entry itself passes upstream. Black, flake8 and targeted mypy passed.
  The test environment used the binary libpq implementation.
- asyncpg: transaction/pool/cancellation/timeout suites ran 101 tests, OK with
  4 skipped. Upstream production code reproduced four transaction failures;
  restored patch passed all 12 transaction tests again. Flake8 passed.
- MySQL: the focused 11 tests passed; upstream production code reproduced
  5 failures and 4 errors, then the restored patch passed again. Connection,
  cursor, pooling and timeout suites ran 94/75/18/22 tests respectively, all OK
  (7/0/0/5 skipped). These are pure-Python aio tests against MySQL 9.6.0.

**Local review is complete; the psycopg and asyncpg branches are submitted.** The asyncpg change deliberately clears stale local registration rather
than promising rollback after every cancelled BEGIN; the draft describes the
reset/reuse boundary. The MySQL patch intentionally retires a cancelled stream
rather than attempting transparent recovery.

## Remaining work before publishing

- MySQL submission is deferred at the user's request. Keep the local patch; do not pursue Oracle registration.
- Upstream CI and maintainer review are not yet confirmed complete.
- psycopg was submitted with both reviewed commits; no history rewrite was needed.

## Round-5 correction: actual vs experimental arms (2026-10-22)

The local patch worktrees above are the actual reviewed patches. Earlier
round-5 "fixed" arms used separate experimental copies, which must not be
attributed to these patches:

- **asyncpg**: `postfix_lease/asyncpg_lease.py --fixed` called
  `install_asyncpg_enter_fix()` (terminate-on-entry), not `ff1bb8d`. The actual
  patch was re-run in a clean interpreter with
  `/home/ava/Worktrees/asyncpg/repo` first on `PYTHONPATH`; each record carries
  the loaded path/version and checks `Transaction.start()` for the identity
  guard. Results are in `postfix_lease/RESULTS.md`: the actual patch leaves a
  same-lease ordinary INSERT returning `ok` with no visible row, makes the
  transaction API refuse explicitly, preserves a legitimate outer transaction
  on a nested failure, and allows explicit reset. The experimental arm refuses
  same-lease reuse but terminates a legitimate outer transaction on a nested
  cancellation; that is a strategy difference, not a property of the patch.
- **psycopg**: the round-5 probe "fix" arm called
  `install_psycopg_enter_fix()` (rollback inside the cancelled `__aenter__`).
  The actual submitted patch is `5f295ab1`/`6650185f`/`c37e8203` on
  `/home/ava/Worktrees/psycopg/tx-cancel`, selected by the interpreter path and
  verified by the presence of `AsyncTransaction._recover_entry_failure`. The
  actual patch is clean at entry cuts in the probe and has zero complete-
  differential violations; results are in `successor_probe/RESULTS.md` and
  `method_compare/RESULTS.md`.
- **MySQL**: the round-5 delay matrix's "patched" build is
  `fix/patched` (`_close_reason`/`InterfaceError`), not `e7a02b6`
  (`_is_poisoned`/`_abort()`/`OperationalError`). A focused re-check of the
  actual worktree is in `postfix_lease/logs/actual-matrix/mysql-*.jsonl`:
  entry/nested/post-commit cancellation poisons the socket, later
  rollback/reset/successor all raise `OperationalError: MySQL Connection not
  available`, and the observer sees no uncommitted row. The old experimental
  build instead raises `InterfaceError: Cancelled during execution; connection
  is not reusable` on rollback.

### New local psycopg partial-send branch

- checkout: `/home/ava/Worktrees/psycopg/partial-send`
- branch: `codex/partial-send-cancel`, based on `origin/master` `99356800`
- files: `psycopg/psycopg/connection_async.py`, `psycopg/psycopg/pq/misc.py`,
  `tests/test_concurrency_async.py`, `docs/news.rst`
- status: **not submitted**, not pushed, no issue/PR/contact. This is separate
  from the transaction-entry branch so it does not pollute PR #1454.
- mechanism: stage-scoped generator tracking via `_WaitGenTracker(gen)` within
  `AsyncConnection.wait()`. If `last_wait` is `None` or `last_wait & WAIT_W`, the
  query was cancelled during the send stage (or prior to read readiness); the
  connection is closed immediately (`self.pgconn.finish()`) and `_INTERRUPTED` is
  re-raised. It avoids `PQflush()` active-sending false negatives and does not match
  exception strings. Fully sent requests (`WAIT_R`) keep the existing cancel-and-drain
  path. A second interruption during recovery also fail-closes.
- focused tests: six regression tests pass on the C and pure-Python backends:
  continuous backpressure partial send, backpressure relief interleaved with
  cancellation, fully-sent query cancellation, recovery interruption, normal large
  request, and large request server error. The baseline fails the interleaving
  regression with `TypeError: an integer is required` (C) or a committed cancelled
  INSERT (pure Python); the fixed tree passes 6/6 on both backends.
- broader suites: async concurrency 20/20, cursor 574 passed (8 skipped), pipeline
  and waiting 91 passed (5 skipped), transaction 84 passed (1 skipped).
- server cleanup: verified via independent direct observer with bounded wait on
  `pg_stat_activity` and `pg_locks` (dropping to 0), and confirmed table lock
  acquisition (`ACCESS EXCLUSIVE NOWAIT`) with 0 rows committed.
- remaining scope: async `AsyncConnection.wait()` only; sync and pipeline
  partial sends are not covered; the repair is fail-closed and loses the lease.
