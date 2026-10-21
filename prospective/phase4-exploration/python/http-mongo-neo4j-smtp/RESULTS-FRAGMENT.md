# Round-3 Python results fragment: httpx + aiosmtplib (unfrozen exploration, partial batch)

Status: **unfrozen exploration, 2026-10-21.** Not Phase 4 data; no
preregistration; no commits. `research/python-retro/` was read only (engine and
delay proxy imported from there, nothing modified).

Batch dir: `prospective/phase4-exploration/python/http-mongo-neo4j-smtp/`.
Run logs: `logs/run.uyIk2F/` (httpx, complete 4×3),
`logs/run.RpMq5v/` (aiosmtplib, complete 4×3). `logs/run.uyIk2F/triage-pending6.log` (the limit=1/2, server-log and recovery
controls) and `cases/triage_httpx.py` (yield-chain tracing) are the httpx
triage evidence.

## 0. Scope actually executed

| package | version | levels run (reps) | state |
|---|---|---|---|
| httpx | 0.28.1 (httpcore 1.0.9) | B1, B2, B3, M1 × 3 | **complete** |
| aiosmtplib | 5.1.3 | B1, B2, B3, M1 × 3 | **complete** |
| pymongo | 4.19.0 | — | **not covered** (timebox) |
| neo4j | 6.4.1 | — | **not covered** (timebox) |
| curl-cffi | 0.16.3 | — | **not covered** (timebox) |

No containers were started for pymongo/neo4j/curl-cffi. The one GreenMail
container was removed by the runner's EXIT trap after the run.

## 1. Versions, environment, images

- Python **3.13.13** (uv-managed) at `~/dropwise-py-work-r3/venv-py313`;
  `uv 0.11.16`, system `python3 3.14.8` also present.
- Packages (same venv, all pinned): httpx 0.28.1, httpcore 1.0.9, aiosmtplib
  5.1.3, anyio 4.15.1, h11 0.16.0, certifi 2026.10.19, idna 3.20, sniffio
  1.3.1, curl-cffi 0.16.3, pymongo 4.19.0, neo4j 6.4.1 (all five installed;
  only the first two were exercised).
- Time: runs on 2026-10-21, Asia/Tokyo; logs contain UTC timestamps in
  `environment.txt`.
- GreenMail image `greenmail/standalone:2.1.14`,
  id `sha256:1ef95a966418cd09b7ea91d504d8c0826bbe7a2f6e679a75c601a831587c1626`,
  digest `greenmail/standalone@sha256:1ef95a96...c1626`; SMTP 3025 and REST
  8080 published on 127.0.0.1 only; `GREENMAIL_OPTS` with
  `-Dgreenmail.setup.test.all -Dgreenmail.auth.disabled`.
- Images present but **unused**: `mongo:7.0.21`
  `sha256:3d715950d83061ff2fbc910d12d3703212538cacf6b3003e3736fa5c7f51a2e1`
  and `neo4j:5.26.0-community`
  `sha256:5a015e53de1895e7eee1574ae0325cf8c4b89587222778108c594bdd45a474b5`
  (recorded here for the planned-but-not-run targets).
- HTTP server: local asyncio process `http_server.py` on 127.0.0.1, HTTP/1.1
  keep-alive, per-connection request log, JSONL request log plus in-memory
  `/__observe__`; no TLS, no HTTP/2, no external network.
- Pending mechanism (one per package): the read-only `DelayProxy`, 50 ms per
  forwarded chunk in both directions. Ops bounded at 2.0 s, engine timeout
  15 s, reps 3. M1 `Expect.ATOMIC`; pre/post twice each.

**Python 3.14 engine limit (recorded, not a target bug).** On CPython 3.14.8,
`asyncio.tasks._PyTask` (the read-only engine's Task base) is invisible to the
C `asyncio.current_task()`, so `asyncio.wait_for`/`asyncio.timeout` inside a
target raise `RuntimeError: Timeout should be used inside a task`; a shielded
probe reproduced this. The batch therefore uses 3.13.13, which is also the
version previously used for the aiohttp retro. This is an engine/runtime
limitation, not a client observation.

## 2. Prior exposure (do not call the protocol families blind)

- **httpx**: new package. The HTTP/1.1 client family is *not* blind: aiohttp
  was studied in `research/python-retro` and HTTP clients in round 1. The
  package's own cancellation handling is new.
- **aiosmtplib**: new package, but the SMTP protocol and GreenMail service
  family were already studied with async-smtp in rounds 1–2 (false 354 /
  delivered-message shape). This is a different client on a touched
  protocol/service family, not a blind family.
- **pymongo**: not run; the MongoDB *server* family was studied in phase 3
  (MG1 `WriteConflict`). If run, it is a current-driver extension of a touched
  server family, not a blind server family.
- **neo4j 6.4.1**: not run; Neo4j 5.26 and BOLT were studied in round 2 with
  bolt-client. Current-driver extension of a touched server family.
- **curl-cffi**: not run; new package on the already-touched HTTP family.

## 3. httpx 0.28.1

### 3.1 Levels

| level | yields | trials | violations | unknown | unrealized | seconds |
|---|---:|---:|---:|---:|---:|---:|
| B1 | 6 | 18 | 0 | 0 | 0 | 2.90 |
| B2 | 6 | 18 | **3** | 0 | 0 | 10.13 |
| B3 | 6 | 18 | **3** | 0 | 0 | 10.17 |
| M1 | 6 | 18 | **3** | 0 | 0 | 19.23 |

All 9 violations are `pending#6` (rep 1–3). Every other plan passed. B1 0/18
means the plain "cancel and stop" level sees only cancellation, no panic or
timeout. There were 0 baseline errors and 0 unrealized plans.

### 3.2 Concrete observations

M1 `pending#6` (3/3, identical after normalisation):

```
target: cancelled/y6/realized=True
survivor: {'target_logged': True, 'survivor_logged': False,
           'survivor_post_ok': False, 'readback_ok': False,
           'doc_after': 'target'}
observer: {'doc': 'target', 'requests': 1}
verdict: Neither
pre:  {'target_logged': False, 'survivor_logged': True,
       'survivor_post_ok': True, 'readback_ok': True,
       'doc_after': 'survivor'}; observer requests 2
post: {'target_logged': True, ... survivor fields true,
       'doc_after': 'survivor'}; observer requests 3
```

B2/B3 `pending#6` (3/3 each): the survivor/retry operation returned
`ERR post-target TimeoutError:` / `ERR post-b3 TimeoutError:` after the 2 s
bound. The retry **request never reached the server**: with the triage token,
`http-server.jsonl` contains only the target POST; there is no survivor row.

So the target's effect happened (server document `target`, request logged) but
the same client could not issue another request at all; this is not a stale
response read. The M1 survivor and readback are two separate 2 s timeouts, and
the independent observer sees only the target request.

`pending#5` (y7) was legal **Post** in all reps: the target completed
(`target_logged: True`) and the survivor overwrote the same document, as
`Expect.ATOMIC` allows. `pending#1`–`#4` were Pre. Verdicts:
`{'Pre': 12, 'Post': 3, 'Neither': 3}`; no `Unknown`.

### 3.3 Triage (confirmed / contract / harness / unresolved)

1. **Rerun** — subset B2 `plans=6` 2/2 violations; full run 3/3 in B2, B3 and
   M1; the earlier exploratory run also had M1 3/3. Not a one-off.
2. **Shrink** — only yield 6; the target's last await chain is
   `httpx/_client.py:1637 send -> _models.py:979 aread` (response body read).
   Cancelling at yields 3–5 and 7 left `connections=[]`, `queued=0`, and a
   retry succeeded. A plain C-task cancellation during a deliberately delayed
   body read (diagnostic `X-Split-Body`) also cleaned up, so the failing window
   is narrower than "any body-read cancellation".
3. **Controls** (`triage-pending6.log`):
   - limit=1: after cancel `pool = {max:1, conns:[ACTIVE, Request Count: 2],
     queued:1}`; retry timed out and its request is absent from the server log.
   - limit=2: retry **succeeded** on a new connection (`doc=survivor`), while
     the leaked connection stayed `ACTIVE` and `queued=1` on the same client.
     This is a **pool-slot/semaphore leak**, not a client-wide lock or a task
     state leak. With max_connections=1 the client is wedged.
   - `await client.aclose()` on the wedged client returns; a fresh client on the
     same proxy succeeds (server unaffected). Recovery is instance-scoped.
4. **Source (read only now)** — httpx 0.28.1 `AsyncClient.send` catches
   `BaseException` and awaits `response.aclose()`. httpcore 1.0.9
   `AsyncConnectionPool.handle_async_request` keeps the `AsyncPoolRequest` in
   `pool._requests` until `PoolByteStream.aclose()` removes it (pool source
   line ~409). `PoolByteStream.aclose()` sets `self._closed = True`, then
   `with AsyncShieldCancellation(): await self._stream.aclose()`, and only
   then removes the request. A raw `Task.cancel()` delivered at that await
   aborts before the removal; the later `response.aclose()` is a no-op because
   `_closed` is already True. `AsyncShieldCancellation` is
   `anyio.CancelScope(shield=True)`; a 10-line probe showed a raw
   `Task.cancel()` is *not* shielded by it (CancelledError escapes the shielded
   sleep). This matches the observed `ACTIVE` connection plus permanently
   queued request.
5. **Classification: confirmed client-side cleanup defect** (httpcore 1.0.9,
   used by httpx 0.28.1), narrow window (cancel exactly during response
   close/`PoolByteStream.aclose`). User-visible consequence: with
   `Limits(max_connections=1)`, a cancelled request leaves the same
   `AsyncClient` unable to send anything else; the target's effect is already
   on the server. A caller can recover only by closing the client (or by
   having spare pool capacity; with limit=2 the next request succeeds and one
   slot stays leaked).
6. **Known vs new / contract**: public httpx docs indexed here do not promise
   cancellation-safe cleanup or tell the caller to close the client after
   cancelling; the connection-reuse/close obligation is not documented in the
   pages found. Search located related upstream discussions by title only —
   [httpx #3154 "httpx requests cancelling outer tasks"](https://github.com/encode/httpx/discussions/3154),
   [httpcore #844 "Is it really necessary to handle cancellations?"](https://github.com/encode/httpcore/discussions/844),
   and the httpcore commit ["Support async cancellations. (#726)"](https://github.com/encode/httpcore/commit/31a4a56f5d8ca4b848f121b8a497b2155bd3f4cd) —
   but GitHub content could not be fetched from this environment (non-public
   address), so **known-vs-new is unresolved**, not "new". No issue/PR was
   opened or commented on.
7. **Patch contrast: not run (timebox).** A minimal candidate is in
   `PoolByteStream.aclose`: guarantee removal in a `finally` (make the
   `_requests.remove` idempotent), or set `_closed` only after the awaited
   close completes. This is a hypothesis with a source-level mechanism; it was
   **not** validated as original-vs-fixed, so the defect is classified
   confirmed but the fix is untested.

### 3.4 pre/post note and limits

Pre and post are distinguishable (target request present or absent) and both
were stable across the four calibration samples (no Unknown). The Post verdict
for `pending#5` is a legitimate completed target, not a failure. Yields vary
with real scheduling (`pending#4` and `pending#6` both show y6); plan numbers
are engine yield indices, not protocol stages. The observation keeps only
invariant facts; connection ids, raw timings and the full server rows are in
DIAG lines / `http-server.jsonl`.

## 4. aiosmtplib 5.1.3

### 4.1 Levels

| level | yields | trials | violations | unknown | unrealized | seconds |
|---|---:|---:|---:|---:|---:|---:|
| B1 | 4 | 12 | 0 | 0 | 0 | 8.02 |
| B2 | 4 | 12 | **12** | 0 | 0 | 38.42 |
| B3 | 4 | 12 | **12** | 0 | 0 | 38.44 |
| M1 | 4 | 12 | **12** | 0 | 0 | 67.67 |

B2/B3/M1 violated at **every** plan and every rep (`pending#1`–`#4`), not only
at one yield. B1 0/12: cancel-and-stop alone is clean.

### 4.2 Concrete observations

Every B2 and B3 retry failed, in two concrete shapes: 9 of 12 trials (three of
the four plans) returned

```
ERR send-target SMTPDataError: (250, 'OK')     # B2
ERR send-b3 SMTPDataError: (250, 'OK')         # B3
```

and 3 of 12 (`pending#3`) returned `ERR send-* TimeoutError: ` at the 2 s
bound. The `SMTPDataError` cases are immediate exceptions, not the bound. The
uncancelled calibration send returns `(recipients={}, final="OK")`
(aiosmtplib returns a dict of *refused* recipients, empty when all accepted,
plus the final message string), so a successful send and the failing retry are
clearly different values.

M1 `Expect.ATOMIC` adds the independent observer and the mailbox readback.
All **12/12** trials were `Neither`; pre was
`mailbox=['<run> survivor']` and post
`mailbox=['<run> survivor', '<run> target']` (stable, no Unknown). The violation
observations depend on which await was cut:

- `pending#1`–`#3` (y1–y3): mailbox `[]` for both the survivor readback and the
  REST observer, i.e. the target was **not** delivered; survivor send
  `ERR ... SMTPDataError: (250, 'OK')` (y1/y2) or
  `ERR send-survivor TimeoutError:` (y3), and the same-object `NOOP`
  `ERR survivor-noop TimeoutError:` (2 s).
- `pending#4` (y4, the final-reply await): mailbox `['<run> target']`, the
  target **was** delivered, survivor send `SMTPDataError: (250, 'OK')`, NOOP
  timeout again.

So in both shapes the long-lived `SMTP` object is unusable after cancellation,
and the observer confirms no survivor message; when the target completed, the
observer sees exactly the target and no survivor. The 3/12 timeout shape
(`pending#3`) is the same desync taken one step further: the retry does not get
even a mis-associated reply within 2 s, and the `NOOP` also times out (the
server is left waiting inside the DATA exchange).

### 4.3 Triage

1. **Rerun**: 12/12 in both B2 and B3, all four plans × 3 reps. Deterministic.
2. **Source**: `aiosmtplib/protocol.py:execute_data_command` sends `DATA`,
   then reads a start response and requires
   `start_response.code == SMTPStatus.start_input` (354); otherwise it raises
   `SMTPDataError(start_response.code, start_response.message)`. After the
   target was cancelled while its send was in flight, the final `250 OK` of
   that send is still unread on the same connection. The retry's `DATA` reads
   that stale 250 as its start response and raises `SMTPDataError(250, "OK")`.
   The client did not close or realign the connection on cancellation;
   `_response_pending` bookkeeping does not discard the outstanding reply.
3. **User-visible shape**: a cancelled send leaves a long-lived SMTP object
   unusable for further sends — every subsequent send fails with a
   *documented* error type (`SMTPDataError`) but a reply code that belongs to
   the cancelled operation. This is the same **response-association family** as
   the round-2 async-smtp result (false 354 / delivered message), with a
   different client and a different visible symptom: the retry does not even
   reach DATA; it treats the stale final `250` as a rejection. The earlier
   round-2 finding is therefore not a one-client curiosity.
4. **Classification: confirmed client response-association defect**
   (aiosmtplib 5.1.3), new package but touched SMTP/GreenMail family. Contract:
   the docs document `SMTPDataError` "on unexpected server response code" and
   `SMTPServerDisconnected` on connection loss, but no cancellation-safety
   statement; the error is a documented type used for a stale reply, so this
   is a cancellation-handling defect, not a documented promise being met.
5. **Known vs new**: web search found no aiosmtplib cancellation issue; the
   nearest hit was [issue #243 "client.connect() call hangs after server
   timeout"](https://github.com/cole/aiosmtplib/issues/243), unrelated. No
   upstream issue/PR was opened or commented on. Treat as **unresolved
   upstream/new candidate**, not confirmed-by-upstream.
6. **Target delivery**: B2/B3 have no independent observer, so the mailbox was
   not read in those levels. The target's send was cancelled only after the
   server had accepted the message in the shapes seen (the stale 250 exists),
   but target delivery is not independently confirmed until M1's REST observer
   completes. This is the main evidence gap for aiosmtplib.
7. **Patch contrast: not run.** Candidate fix: on `CancelledError` in
   `execute_data_command`/`send_message`, close the connection (or drain the
   outstanding reply and mark the connection unusable) so the next send starts
   from a known state. Untested.

## 5. Executions vs independent root causes

| package | executions counted | violations | independent root causes |
|---|---:|---:|---:|
| httpx | 72 (4 levels × 18 trials) | 9 (all `pending#6`) | **1** |
| aiosmtplib | 48 (4 levels × 12 trials) | 36 (B2/B3/M1 all plans) | **1** |
| pymongo / neo4j / curl-cffi | 0 | 0 | 0 (not covered) |

Execution counts are not root-cause counts: 9 httpx violations are one cleanup
defect, 36 aiosmtplib violations are one stale-reply/desync defect.

## 6. Cost

Wall clock about 25–30 minutes (2026-10-21 21:43–22:1x Asia/Tokyo),
including ~3 min for `uv` venv/install (one detour: Python 3.14 was abandoned
after the engine/`current_task` incompatibility), ~5 min for the httpx ladder,
~8 min for httpx triage/controls, ~7 min for aiosmtplib (B2/B3 are slow because
every trial waits for the 2 s bound on the cancelled target's final reply), and
the rest for artifacts. Code added: `cases/support.py` (~140 lines),
`http_server.py` (~260), `cases/httpx_case.py` (~280),
`cases/aiosmtplib_case.py` (~280), `cases/triage_httpx.py` (~140), two runners.
No commits; no changes under `research/python-retro/` or any historical
results.

## 7. Not covered / limits

- **pymongo 4.19.0**: no replica set started, no scenario, no executions. The
  planned `ClientSession`-vs-physical-connection distinction is written in
  SCENARIO.md only. MongoDB server family was touched in phase 3, so do not
  call it blind if it is run later.
- **neo4j 6.4.1**: no container, no scenario, no executions; Neo4j/BOLT family
  touched in round 2, current driver not exercised.
- **curl-cffi 0.16.3**: no scenario, no executions. Native-callback coverage
  limits (libcurl continues in its own thread; the yield ladder sees only the
  Python await) are recorded as a design constraint, not as evidence.
- **aiosmtplib** M1 completed but only after the first report draft; the
  observer/mailbox evidence is now in §4. B1 was clean, B2/B3/M1 are all
  12/12; no patch contrast, no upstream issue content.
- **httpx**: one loopback server, one proxy, HTTP/1.1, no TLS/HTTP2/proxy/
  pipelining, one client version; no patch contrast; upstream content not
  fetched (blocked), so known-vs-new is unresolved. The failing window is
  narrow: plain C-task cancellation at body-read and at other yields was
  clean; the engine hits it deterministically by cancelling at every yield.
- The 2 s bound was ruled out for httpx by the server log (retry absent) and
  the limit=2 / aclose / fresh-client controls; for aiosmtplib the failures
  are immediate `SMTPDataError`, not timeouts.
- No blind protocol: prior exposure is stated in §2. No randomisation, no
  repeated independent server versions, no production traffic.
