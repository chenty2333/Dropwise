# Round-3 Python results fragment: redis-py 8.1.0 (unfrozen exploration)

Status: **unfrozen exploration, 2026-10-21.** Not Phase 4 data; no
preregistration; no commits/pushes. `research/python-retro/` was read only
(engine + delay proxy imported, nothing modified); all new artifacts are under
`prospective/phase4-exploration/python/redis/`.

Batch dir: `prospective/phase4-exploration/python/redis/`.
Primary logs: `logs/run.b4e53b/` (16 level runs, all exit 0, stderr empty).
Pre-audit logs: `logs/run.d72cdb/` (first ladder, taken while the adapter was
being changed to the structured `OpResult` contract; **its B2/B3 numbers are
invalid and none of its numbers are used here** — see §8).
Recovery contrasts: `logs/run.b4e53b/recover-*.log`; per-plan probes:
`logs/run.b4e53b/probe-*.log`; adapter selftest:
`logs/run.b4e53b/selftest-adapter.log`.

## 0. Scope executed

| unit | reuse unit | target operation | levels run (reps) | state |
|---|---|---|---|---|
| `pool/tx` | `ConnectionPool(max_connections=1)` + `Redis(connection_pool=…)` | real `WATCH` → `MULTI` → `SET key target` → `EXEC` through `pipeline(transaction=True)` | B1, B2, B3, M1 × 3 | complete |
| `pool/set` | same pool | one plain `SET key target` | B1, B2, B3, M1 × 3 | complete |
| `direct/tx` | one held `Redis(single_connection_client=True)` connection | the same transaction as four real commands: `WATCH`, `MULTI`, `SET key target`, `EXEC` (bounded `DISCARD` cleanup) | B1, B2, B3, M1 × 3 | complete |
| `direct/set` | same held connection | one plain `SET key target` | B1, B2, B3, M1 × 3 | complete |

Not run: pymongo/neo4j/curl-cffi (other batches), PubSub, cluster,
`BLPOP`/blocking commands, TLS. No Phase 4 table was filled.

## 1. Versions, environment, digest

- Python **3.13.13** (uv venv), `/home/ava/dropwise-py-work-r3/venv-redis313`;
  `redis==8.1.0` (installed with `uv pip install --python … 'redis==8.1.0'`;
  `redis.__version__ == "8.1.0"` recorded in every log).
- Server image **`redis:8-alpine`**, id
  `sha256:9d317178eceac8454a2284a9e6df2466b93c745529947f0cd42a0fa9609d7005`
  (single tag/digest, verified with `docker image inspect`); server reports
  **Redis 8.8.0**. Container `dw-redis-r3`, created with
  `docker run -d --rm --name dw-redis-r3 -p 127.0.0.1::6379 redis:8-alpine
  redis-server --save '' --appendonly no --appendfsync no`; host port
  `127.0.0.1:32851` (loopback only, ephemeral); `appendonly=no`,
  `save` disabled; no password. Removed with `docker stop dw-redis-r3`
  (container is `--rm`); `docker ps -a` shows no `dw-redis` container and
  port 32851 is closed afterwards.
- Pending mechanism (one for all units/levels): `DelayProxy` from the
  read-only `research/python-retro/cases/common.py`, **0.05 s on every
  forwarded chunk, both directions**, wrapping only the tested client; the
  observer connects directly to the server. Op bound 2.0 s, engine timeout
  12 s, `reps=3`, `Expect.ATOMIC`, two calibration samples per side.
- Time: 2026-10-21 22:45–22:52 Asia/Tokyo (`environment.txt` has UTC).

## 2. Prior exposure (not a blind family)

redis-py was studied in `research/python-retro` (4.5.2/4.5.3/4.5.4; R1
`BLPOP` / CVE-2023-28859, R2 `pipeline` / CVE-2023-28858). In 4.5.3 the
cancelled non-pipeline command's reply stayed on the connection and the
survivor read it (upstream #2665); 4.5.4 added disconnect-on-cancellation.
This round is the **current-version extension of a touched client family**
(`targets.md` §5.2), not a blind package. 8.1.0 is expected to contain that
hardening, and the ladder was designed to hit the same "reply pending" window
(plain unit, `pending#1`) and the transaction window (tx unit).

## 3. All four units

### 3.1 Levels (primary run `run.b4e53b`)

| unit | level | K yields | trials | violations | unknown | unrealized | baseline errors | verdicts |
|---|---|---:|---:|---:|---:|---:|---:|---|
| pool/tx | B1 | 4 | 12 | 0 | 0 | 0 | 0 | — |
| pool/tx | B2 | 4 | 12 | 0 | 0 | 0 | 0 | — |
| pool/tx | B3 | 4 | 12 | 0 | 0 | 0 | 0 | — |
| pool/tx | M1 | 4 | 12 | 0 | 0 | 0 | 0 | Pre 6, Post 6 |
| pool/set | B1 | 1 | 3 | 0 | 0 | 0 | 0 | — |
| pool/set | B2 | 1 | 3 | 0 | 0 | 0 | 0 | — |
| pool/set | B3 | 1 | 3 | 0 | 0 | 0 | 0 | — |
| pool/set | M1 | 1 | 3 | 0 | 0 | 0 | 0 | Post 3 |
| direct/tx | B1 | 4 | 12 | 0 | 0 | 0 | 0 | — |
| direct/tx | B2 | 4 | 12 | 0 | 0 | 0 | 0 | — |
| direct/tx | B3 | 4 | 12 | 0 | 0 | 0 | 0 | — |
| direct/tx | M1 | 4 | 12 | 0 | 0 | 0 | 0 | Pre 9, Post 3 |
| direct/set | B1 | 1 | 3 | 0 | 0 | 0 | 0 | — |
| direct/set | B2 | 1 | 3 | 0 | 0 | 0 | 0 | — |
| direct/set | B3 | 1 | 3 | 0 | 0 | 0 | 0 | — |
| direct/set | M1 | 1 | 3 | 0 | 0 | 0 | 0 | Post 3 |

120 trial executions (30 per level), 10 distinct cancellation points, 0
violations, 0 unknown, 0 unrealized plans, 0 baseline errors, 0 non-zero exit
statuses, all `level-*.err` empty. `reps` = 3. M1 pre/post samples agreed in
all 4 units (no `Unknown`); pre ≠ post in every unit, so no degenerate
observation.

### 3.2 Concrete values (M1, identical across the 3 reps)

Calibration (all units):

```
pre  = survivor {pre_read: ["ok", null],   set: ["ok", true], own: ["ok","survivor"]}
       observer {value: ["ok","survivor"]}
post = survivor {pre_read: ["ok","target"], set: ["ok", true], own: ["ok","survivor"]}
       observer {value: ["ok","survivor"]}
```

`pre_read` is the acquire-time value of the same key read through the same
pool/held connection before the survivor write; `observer` is a separate
direct `redis.asyncio` connection. The observer always saw `survivor`, never a
stale `target`/`OK`/`QUEUED`, and the survivor's own readback never showed the
target's stale reply.

Per plan (M1, 3/3 reps):

| unit | plan | target outcome | verdict | observation (survivor pre_read / observer) |
|---|---|---|---|---|
| pool/tx | pending#1 | cancelled (WATCH read) | Pre | `null` / `survivor` |
| pool/tx | pending#2 | cancelled (optimistic GET read) | Pre | `null` / `survivor` |
| pool/tx | pending#3 | cancelled (packed MULTI/SET/EXEC reply) | **Post** | `target` / `survivor` |
| pool/tx | pending#4 | cancelled (post-EXEC cleanup/release suspension) | **Post** | `target` / `survivor` |
| direct/tx | pending#1 | cancelled (WATCH read) | Pre | `null` / `survivor` |
| direct/tx | pending#2 | cancelled (MULTI read) | Pre | `null` / `survivor` |
| direct/tx | pending#3 | cancelled (SET `+QUEUED` read) | Pre | `null` / `survivor` |
| direct/tx | pending#4 | cancelled (EXEC read) | **Post** | `target` / `survivor` |
| pool/set | pending#1 | cancelled (SET reply pending) | **Post** | `target` / `survivor` |
| direct/set | pending#1 | cancelled (SET reply pending) | **Post** | `target` / `survivor` |

The Post rows are the intended "a cancelled operation may legitimately
complete" case (`Expect::Atomic`): the transaction or the SET was executed by
the server before the cancellation was delivered, and the survivor's write
still won. No plan produced `Neither`.

`pool/tx pending#3` and `direct/tx pending#4` are the strongest positive
evidence for the transaction: `pre_read == "target"` means `EXEC` really ran
before the cancelled caller read the reply, and the next user's `SET`/`GET` on
the same unit still returned `survivor`. `direct/tx pending#3` is the opposite:
the transaction's `SET` was only `+QUEUED`, the cancellation arrived before
`EXEC`, and `pre_read == null` — the queued write did not leak into the next
user's world.

### 3.3 Reconnect evidence (diagnostics, not judged)

Post-survivor `CLIENT ID` vs setup `CLIENT ID` (`probe-*.log`, one trial per
plan; the same pattern appears in the ladder DIAG lines):

- `pool/tx`: #1, #2, #3 changed; #4 unchanged (the cancel at #4 landed after
  the transaction and its reply were complete, so nothing had to be
  repaired).
- `direct/tx`: #1–#4 all changed (every cancellation happened while a reply
  was pending; the socket was closed and the next command reconnected).
- `pool/set`, `direct/set`: #1 changed.

Pool state after every survivor stayed `{available: 1, in_use: 0, max: 1}` in
all four units — no leaked checkout, no `MaxConnectionsError`, no
`Connection has data` reconnect on checkout. The reconnects are the client's
own cancellation recovery (close the socket on `CancelledError`), i.e. legal
self-healing recorded rather than blocked; the survivor was never silently
served by a *pre-survivor* replacement connection.

### 3.4 Recovery contrast (`--recover`, requirement 4)

After cancelling at every plan of every unit, the same unit was asked to
recover explicitly; all 10/10 showed new responses only:

```
DISCARD  -> err ResponseError: DISCARD without MULTI   (no MULTI open)
UNWATCH  -> ok True
SET key recover-<k> -> ok True
GET key            -> ok b"recover-<k>"       (never b"target"/b"OK"/b"QUEUED")
PING               -> ok True
observer GET       -> ok b"recover-<k>"
```

The `DISCARD` error is the *clean* answer: there is no transaction state to
discard. In a leftover-MULTI world `DISCARD` would return `OK`; a stale reply
would surface as `OK`/`QUEUED`/`target` in the tagged `GET`. Neither happened.
The only reconnects in this probe are again the client's cancellations
(`client_id` changed for 9/10 plans; unchanged only for `pool/tx pending#4`,
which cancelled after the work was done).

## 4. Contract and zero statement

Redis documents `MULTI`/`EXEC`/`WATCH`/`DISCARD` semantics and redis-py
documents `pipeline(transaction=True)` as wrapping commands in `MULTI`/`EXEC`;
neither documents a cancellation/timeout/abort guarantee for an asyncio task
cancelled mid-command. The judged contract is therefore the caller-level one
(`Expect.ATOMIC`): after the target is cancelled, the next operation on the
same pool/held connection must either see the pre world (target not committed)
or the post world (target committed, then overwritten by the survivor), must
not receive the cancelled target's reply, and must not fail or hang.

**No violation observed in redis-py 8.1.0 (redis.asyncio), for the four units,
the loopback Redis 8.8.0 service, the listed operations and the realised
cancellation points** (120 executions, 10 points, 3 reps; 0 violations, 0
unknown, 0 unrealized, 0 baseline errors). This is a limited-scope negative
result, not "clean" and not "proven safe".

## 5. Why zero (mechanism found in the installed source, used only for triage)

- `redis/asyncio/connection.py`: `read_response()` has
  `except BaseException: if disconnect_on_error: await self.disconnect(nowait=True); raise`
  (around lines 1320–1330) and `send_packed_command()` has the same
  BaseException → `disconnect(nowait=True)` path (lines ~1200–1210). A
  cancellation while a reply is pending therefore closes the socket; the
  next command reconnects. This is the 4.5.4-era hardening, now in the
  connection layer, and it explains the client-ID changes in §3.3 without a
  stale reply being served.
- `redis/asyncio/client.py` `Pipeline.reset()` (lines ~1848–1880) sends
  `UNWATCH` when still watching, catches `asyncio.CancelledError` around that
  read (disconnect, re-raise), and shields the pool release; `execute()`'s
  `finally` calls `reset()`. So a cancelled pool transaction does not hand a
  connection in MULTI/WATCH state to the next borrower in our plans.
- `ConnectionPool.get_connection()` / `ensure_connection()` peeks
  non-destructively (`can_read()` checks the parser buffer/EOF without I/O)
  and reconnects if data is pending; `release()` drops a connection whose
  `should_reconnect()` is set. This is the self-healing path noted in the
  retro (`RESULTS.md` §5.1); it was available but not needed, because the
  cancellation had already disconnected before release.
- Direct mode has no pool: the target's own bounded `finally: DISCARD` covers
  the "MULTI open, EXEC not sent" window; where the read was cancelled the
  connection was already disconnected and the DISCARD ran on a fresh socket
  (recorded as `ResponseError: DISCARD without MULTI`, expected, not raised).

## 6. Known vs new; upstream status

- **No new issue found.** The only relevant upstream shape surfaced by the
  read-only search is redis-py issue **#4121** ("`Pipeline.reset()` leaks a
  pooled connection if cancelled during the UNWATCH read") with PRs
  **#4122/#4123**. Full `github.com` pages could not be fetched from this
  environment (DNS resolves to a non-public address; only search-result
  snippets were available), so the issue text was not read in full. The
  installed 8.1.0 source contains exactly the fix shape described by the PR
  titles (CancelledError handling around the UNWATCH read + shielded release),
  which is consistent with 8.1.0 post-dating the fix; **not ladder-verified**
  here because it needs a *second* cancellation during cleanup (see §7).
- Historical, already known: CVE-2023-28858 (pipeline, 4.5.3) and
  CVE-2023-28859 (non-pipeline, 4.5.4) from the retro; 8.1.0 did not
  reproduce either shape.
- No patch was written: there is no violation to contrast, and producing a
  synthetic patch would not be evidence about the version under test.

## 7. Coverage gaps

- **One cancellation per trial** (engine semantics). A second cancel during
  `Pipeline.reset()`'s UNWATCH read — the #4121 window, reachable in
  production via an `asyncio.timeout` that expires during cleanup — is not a
  ladder plan. Only source inspection covers it; a double-cancel probe is the
  next step if this unit is promoted.
- Plain-SET units have exactly **one** suspension under this pending
  mechanism (one plan per rep); the historical "reply pending" window is
  covered but coarsely.
- No blocking command (`BLPOP`), no PubSub/push, no cluster, no
  `single_connection_client` + `pipeline()` combination (that combination
  would use a second connection, which is why it is excluded), no TLS, no
  `SELECT`/`CLIENT SETNAME` session state, no server restart/failure
  injection, no concurrent borrowers on the pool (concurrency 1 by design).
- B1/B2/B3 judge only an escaping error (B1 target error/timeout;
  B2/B3 structured `OpResult(ok=False)` or an escaping exception), not
  semantic corruption; M1 is the semantic judge.
- If the independent observer itself failed in both calibration samples, the
  observer field would be equally degenerate on both sides; setup pings the
  observer and no such failure occurred.
- The redis batch was run after the Python engine gained the structured
  `OpResult` contract; the first ladder (`run.d72cdb`) straddled the adapter
  change and is not used.

## 8. Adapter audit (follow-up to aiomysql/asyncpg error swallowing)

The redis adapter was checked against the same failure-reporting bug class:

- `target()` raises the original exception from any failed stage, so an
  uncancelled failing target is an engine `baseline_error` (and a B1 target
  error), never a silently "completed" target.
- `same()` / `different()` return
  `dropwise_py.engine.OpResult(ok=…, stage=…, value=…, error=…)`; the partial
  stage dict (`watch`/`read`/`multi`/`queue`/`exec`/`discard` results) is kept
  in `value` on failure, so a failed stage is visible and cannot be counted
  as a successful retry. Only `CancelledError` propagates.
- Setup/warm-up/observer-connect raise on failure (they are not swallowed);
  diagnostic steps (`CLIENT ID`, pool counts, tx-state probe, close) are
  bounded, recorded in `DIAG` lines, and never enter the judged observation.
- `cases/selftest_adapter.py` (no service, no socket) passes:
  `OpResult(ok=False)` → B2 violation, raised exception → B2 violation,
  0.5 s timeout → B2 violation, `OpResult(ok=True)` → no violation,
  target failure → baseline error (B1 and B2), and a `None` return is
  invisible to the engine (the negative control that motivates the contract).
- `run.b4e53b/` is the post-audit ladder for all four levels; `run.d72cdb/`
  was the pre-audit ladder (kept, B2/B3 invalid, numbers unused). No Expect
  was changed.

## 9. Paths

- `prospective/phase4-exploration/python/redis/SCENARIO.md` — units, pending,
  contract, observation, limits.
- `prospective/phase4-exploration/python/redis/cases/redis_case.py` — scenario,
  diag/probe/recover modes.
- `prospective/phase4-exploration/python/redis/cases/common.py` — bound/
  sanitise helpers + retro imports.
- `prospective/phase4-exploration/python/redis/cases/selftest_adapter.py` —
  no-service failure-contract test.
- `prospective/phase4-exploration/python/redis/run-redis.sh` — ladder runner.
- `prospective/phase4-exploration/python/redis/logs/run.b4e53b/` — primary:
  `environment.txt`, `level-<mode>-<unit>-<level>.{log,err,status}` (16),
  `selftest-adapter.log`, `recover-*.log` (4), `probe-*.log` (4).
- `prospective/phase4-exploration/python/redis/logs/run.d72cdb/` — pre-audit
  ladder, kept for the record, marked superseded in its `environment.txt`.

## 10. Cost

~0.6 h wall clock including environment setup, 4 units × 4 levels, triage
reading of the installed client source, recovery contrast and the adapter
audit. 120 ladder trial executions + 16 calibration samples + 4 count runs;
16 level processes, all exit 0. No container/server left running.
