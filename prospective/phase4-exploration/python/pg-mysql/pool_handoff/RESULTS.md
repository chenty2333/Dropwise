# Pool acquire/checkout delivery-window cancellation (round 5, Work 3)

Question: a resource/quota has been assigned to a caller, but the caller has not
yet received the lease/guard.  If the caller is cancelled in that window, does a
successor borrow, is capacity recovered, and are existing legitimate users
affected?

This is tested against the mature public delivery hooks, without presupposing a
bug:

- psycopg_pool 3.3.3: the `check` callback runs after a connection is assigned
  and before the waiting client receives it.  A cancellation in the callback is
  caught by `_getconn_with_check_loop`, which calls
  `_putconn(conn, from_getconn=True)`; the connection returns to the pool.
- asyncpg 0.32.0: `Pool._acquire` pops a holder, then `ch.acquire()`; a
  cancellation puts the holder back.  The public `setup` callback runs on every
  holder acquire and is used here as the delivery hook.

## Scenario

`delivery_case.py` keeps a legitimate user holding connection A (session GUC
`dw.legit=keeper`, temporary table `dw_legit_tmp`), while connection B is
assigned to a target caller.  Shapes:

- `normal`: B is delivered and used;
- `early`: cancel while B is still waiting in the pool queue;
- `delivery`: cancel while the assigned B is in the public delivery callback;
- `late`: cancel after B was successfully delivered.

After cancellation each trial waits for the pool to settle, then borrows a new
connection, checks the legitimate user's pid/session state, the target's write,
capacity, and the final server connection count.

## Results (2 reps per shape)

Psycopg pool, min_size=1/max_size=2, local PostgreSQL 18.4:

| shape | cut realised | next acquire | next same conn | final pool | legit user | issues |
|---|---|---|---|---|---|---|
| normal | — | ok | yes | size 2 / available 2 / waiting 0 | pid, GUC, temp intact | none |
| early | yes | ok | **no** | size 2 / available 2 / waiting 0 | intact | none |
| delivery | yes | ok | **yes** | size 2 / available 2 / waiting 0 | intact | none |
| late | yes | ok | yes | size 2 / available 2 / waiting 0 | intact | none |

Asyncpg pool, min_size=1/max_size=2:

| shape | cut realised | next acquire | next same conn | final pool | legit user | issues |
|---|---|---|---|---|---|---|
| normal | — | ok | yes | size 2 / idle 2 | pid, GUC, temp intact | none |
| early | yes | ok | **no** | size 2 / idle 2 | intact | none |
| delivery | yes | ok | **no** | size 2 / idle 2 | intact | none |
| late | yes | ok | no | size 2 / idle 2 | intact | none |

`next same conn = no` for asyncpg delivery is not a leak: the holder was
closed/reconnected by the pool, and the next checkout is a fresh holder.  The
important invariant is capacity and the legitimate user, both intact.
Psycopg delivery could return the same B because its cancellation path returns
the connection and the pool's reset makes it reusable.

Runtime: psycopg 0.024-0.033 s/trial; asyncpg 0.187-0.197 s/trial (2 reps,
including setup/cleanup).  Logs: `pool_handoff/logs/`.

## Protection mechanism and covered window

No defect was found in this window.  The window **is** covered by the mature
implementations:

- psycopg_pool's `_getconn_with_check_loop` catches `BaseException` around the
  check/delivery and returns the assigned connection with `from_getconn=True`;
  the pool cannot leak the assignment on cancellation.  The delivery callback
  runs before the waiting client's future is completed, so a cancellation there
  is still inside the protected try.
- asyncpg's `_acquire` wraps `ch.acquire()` and puts the holder back on
  `Exception`/`CancelledError`; the public `setup` callback therefore runs
  while the pool still owns the return path.

This is a positive result about the implementations, not a negative bug hunt:
the window is real, but these versions close it.  The limit is one host, one
warm pool, no forced concurrent acquisition storm, and only these two pool
versions.  A clean next checkout does not by itself prove that a **same-checkout
retry** after an interrupted transaction entry is safe; that is the separate
`postfix_lease/` and Work 1/2 boundary.
