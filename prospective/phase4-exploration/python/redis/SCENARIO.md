# SCENARIO — redis-py 8.1.0 (round 3, unfrozen exploration)

Not Phase 4 data, no preregistration, no commits. `research/python-retro/`
was read only: the engine (`dropwise_py`) and the delay proxy
(`cases/common.py`) are imported from there unchanged. The only repo edits in
this batch are new files under `prospective/phase4-exploration/python/redis/`.

One `Scenario` subclass (`cases/redis_case.py`) with four **separate** reuse
units, selected by `--mode {pool,direct} --unit {tx,set}`; a fresh scenario
instance per run. Units are never combined into one ladder.

## Pending mechanism (all units, all levels)

`DelayProxy` from `research/python-retro/cases/common.py`, **50 ms on every
forwarded chunk in both directions**. It wraps only the tested client; the
observer connects directly to the server. No server-side `SLEEP`, no fixed
settle, no second mechanism. Ops are bounded at 2.0 s (`bound()`); the engine
timeout (12 s) is a backstop so a hang becomes a recorded value.

A `SET`/`WATCH` reply therefore arrives after the client has suspended; a plan
is the target's k-th suspension (`Task.cancel()`), so `pending#1` for the
plain unit is exactly "waiting for the reply of the only round trip", the same
window as the historical redis-py BLPOP case. Pool-transaction targets have
4 suspensions (WATCH read, optimistic GET read, the packed MULTI/SET/EXEC
reply read, and one post-EXEC cleanup/release suspension); direct transactions have 4 (one per command read).

## Units

| unit | reuse unit | target (state-establishing operation) | cleanup of the target |
|---|---|---|---|
| `pool/tx` | `redis.asyncio.ConnectionPool(max_connections=1)` + `Redis(connection_pool=…)` | real transaction: `client.pipeline(transaction=True)`, `WATCH key` → optimistic `GET` → `MULTI` → `SET key target` → `EXEC` | the pipeline context manager: `reset()` sends `UNWATCH` if still watching, then releases the connection back to the pool |
| `pool/set` | same pool | one plain `SET key target` | none needed; the command's connection is released by `execute_command`'s `finally` |
| `direct/tx` | one held `Redis(host=…, single_connection_client=True)` connection | real transaction issued as four commands on the held connection: `WATCH`, `MULTI`, `SET key target`, `EXEC` | `finally`: bounded `DISCARD` while MULTI is known open (a DISCARD after a committed EXEC is an expected `ResponseError` and is recorded, not raised) |
| `direct/set` | same held connection | one plain `SET key target` | none |

**What is a real transaction and what is not.** In `pool/tx`, `pipe.watch()` is
a real `WATCH` to the server, `pipe.multi()` sets the client-side marker, and
`pipe.execute()` packs `MULTI` + `SET` + `EXEC` into one write; `EXEC` returns
the command result, so the server executed a genuine WATCH/MULTI/EXEC
transaction. It is not a non-transactional pipeline (`transaction=True`).
In `direct/tx` the same four commands are sent as separate round trips with
`execute_command`, because `Redis(single_connection_client=True).pipeline()`
checks out a *second* connection from the underlying pool; using it would
leave the held connection untested. No MULTI is simulated client-side in
either unit.

## Survivor, observer, pre/post

Survivor (all units), on the same pool/held connection as the target:

1. `GET key` — acquire-time value of the same object (the state handed over),
2. `SET key survivor`,
3. `GET key` — own readback (must be `survivor`).

Observer: a separate `redis.asyncio.Redis` client connected **directly** to
the server, never through the tested pool and never through the proxy:
`GET key` plus, in diagnostics only, a MULTI-count view derived after the
judged read.

`Expect.ATOMIC`: `pre` = target never ran (pre_read `None`, observer
`survivor`); `post` = target committed, then the survivor overwrites
(pre_read `target`, observer `survivor`). Pre and post differ only in
`pre_read`, so the observation is not degenerate. A cancelled transaction may
legitimately be `Pre` (not committed / discarded) or `Post` (committed before
the cancellation was delivered); both are allowed. A `Neither` observation is
the violation shape: the cancelled target's stale reply served to the
survivor (`OK`/`QUEUED`/an array instead of a value), the connection left in
MULTI/WATCH so the survivor's write is queued or lost, a survivor error or
2 s timeout, or an observer that never sees the survivor write.

Judged observation (engine): `{"survivor": {pre_read, set, own}, "observer":
{value}}`, sanitised (key stem and loopback port replaced). Client IDs, pool
counts, connection flags, the post-survivor MULTI/PING/EXEC probe and close
statuses are **diagnostics only** and never enter the judged value, so a legal
transparent reconnect cannot turn a legal Pre/Post into `Neither`.

## Adapter failure contract (round-3 audit)

`target` raises the original exception on any failed stage (so an uncancelled
target failure is an engine `baseline_error`, not a completed target);
`same`/`different` return `dropwise_py.engine.OpResult(ok=…, stage=…,
value=…, error=…)`, including the failed stage and the partial stage dict,
and only `CancelledError` propagates. No target/same/different path returns
`None` or a bare dict that hides a failed stage; cleanup (DISCARD, close) is
bounded and its outcome is recorded in the log. `cases/selftest_adapter.py`
is a no-service test of this contract (structured failure, raised failure,
timeout → B2 violations; success → none; `None` is invisible to the engine,
which is why the adapter never returns it).

## Levels and diagnostics

`level_b1`, `level_retry(B2, same)`, `level_retry(B3, different)`,
`explore_observed(Expect.ATOMIC, reps=3)`; B2 retries the same unit/key/value,
B3 does the same kind of operation on a different key (tx → tx on the b3 key;
plain → `INCR` on the b3 key). Per run: uncancelled target for K yields, two
`pre` and two `post` calibration samples, then K plans × 3 reps. `reps` = 3.

Extra diagnostics (logged, not judged):

* setup and post-survivor `CLIENT ID` (a changed ID with an otherwise clean
  observation is the client's own transparent reconnect, i.e. legal
  self-healing, recorded rather than blocked);
* pool `available`/`in_use`/`max` before and after the survivor;
* post-survivor `MULTI; PING; EXEC` probe on the same unit, run **after** the
  independent observer read, to expose a connection left in MULTI (a clean
  unit answers `PING` queued then `EXEC` `[PONG]`; a leftover MULTI makes the
  new MULTI fail and the probe surface a `ResponseError`);
* `--recover k`: explicit recovery contrast on the same unit after cancelling
  at plan k — `DISCARD` (clean unit: `ResponseError: DISCARD without MULTI`;
  leftover MULTI: `OK`), `UNWATCH`, tagged `SET recover-k` + `GET` (a stale
  reply would show an old value/`OK`/`QUEUED`; a new reply shows `recover-k`),
  `PING`, and the observer's view of the tag.

## Contract search / prior exposure

* Redis transaction semantics (`MULTI`/`EXEC`/`WATCH`, `DISCARD`) are
  documented by Redis; redis-py documents `pipeline(transaction=True)` as
  wrapping commands in `MULTI`/`EXEC`. Neither documents any cancellation,
  timeout, drop or abort guarantee for an asyncio task cancelled mid-command,
  so the *atomicity expectation* used here is the caller-level contract, not a
  vendor promise (contract-external for cancellation semantics).
* redis-py 4.5.3/4.5.4 and the pipeline CVE-2023-28858 / CVE-2023-28859 family
  were already studied in `research/python-retro` (R1 BLPOP, R2 pipeline):
  4.5.3 left the cancelled non-pipeline reply on the connection and the
  survivor read it; 4.5.4 fixed it by disconnecting on cancellation. This
  batch is a **current-version extension of a touched client family**, not a
  blind family, and 8.1.0 is expected to contain that hardening.
* Upstream search (read-only; full github.com pages were blocked by the
  network policy, only search-result snippets were available) surfaced
  redis-py issue #4121 "`Pipeline.reset()` leaks a pooled connection if
  cancelled during the UNWATCH read" and PRs #4122/#4123. The installed
  8.1.0 source contains exactly that handling: `Pipeline.reset()` catches
  `asyncio.CancelledError` around the UNWATCH read, disconnects, re-raises,
  and shields the pool release. This is a known upstream shape, not a new
  finding; see the limits below for what the single-cancel ladder does not
  exercise.

## Known limits

* The engine delivers **one** cancellation per trial. Cancelling *again*
  while `Pipeline.reset()` is awaiting the UNWATCH reply (the #4121 shape, as
  an `asyncio.timeout` expiring during cleanup would) is therefore not a
  ladder plan; the recovery probe only shows the post-cancellation state, and
  the source inspection above is the only evidence for that path.
* The plain-SET unit has exactly one suspension under this pending mechanism,
  so its ladder is one plan per rep; that plan is the historical
  "reply pending" window but the coverage is coarse.
* No blocking command (`BLPOP`), no PubSub, no cluster/`RedisCluster`, no
  `single_connection_client` + `pipeline()` combination, no TLS, no
  reconnection-during-retry test, no multi-database `SELECT`, no `CLIENT
  SETNAME` session state (WATCH/MULTI is the session-state unit).
* Observer noise is normalised by the judge; if the observer itself failed in
  both calibration samples the observer field would be equally degenerate on
  both sides (setup pings the observer, so this did not occur).
* B1/B2/B3 only judge an escaping error (`target` error/timeout for B1;
  structured `OpResult(ok=False)` or an escaping exception for B2/B3). B2/B3
  do not judge semantic corruption; M1 does.
