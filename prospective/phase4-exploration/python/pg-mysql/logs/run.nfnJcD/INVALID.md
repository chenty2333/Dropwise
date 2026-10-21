# INVALID — asyncpg pool ladder in this run directory

`asyncpg-pool-fixed.txt` in this directory is **invalid as a target finding**.
Its B1 (9/12) and B2 (12/12) violations are an adapter artifact:

* `_tx_write()` called `conn.is_in_transaction()` / `conn.get_server_pid()` on a
  pool handle **after** `pool.release()`, when asyncpg has already marked it
  released.  The resulting
  `InterfaceError: cannot call Connection.is_in_transaction(): connection has
  been released back to the pool` replaced the target's `CancelledError` (and
  even failed the uncancelled baseline), so B1/B2 counted diagnostic failures
  as violations.
* The diagnostic path is now guarded (`_safe_in_tx` / `_safe_pid`); the
  corrected rerun is in `logs/run.1HLY38/asyncpg-pool-fixed.txt`
  (B1/B2/B3/M1 = 0 violations, 0 baseline errors).

The `psycopg-pool-fixed.txt` and `psycopg-direct-fixed.txt` ladders in this
directory were produced before that asyncpg-only bug and are valid; they are
also reproduced in `run.1HLY38` for a single coherent run.  `run.KqRbB4/` was
never modified.
