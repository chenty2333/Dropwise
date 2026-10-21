# Round-3 Python addendum: aiohttp 3.14.5 and curl-cffi 0.16.3 (unfrozen exploration)

Status: **unfrozen exploration, 2026-10-21.** Not Phase 4 data; no preregistration;
no commits, pushes, issues or upstream contact. This file is *additive*: the
existing `RESULTS-FRAGMENT.md` and `logs/run.uyIk2F/` (httpx) and
`logs/run.RpMq5v/` (aiosmtplib) are unchanged, and no frozen Phase 4 data was
touched. `research/python-retro/dropwise_py/engine.py` and
`research/python-retro/cases/common.py` were imported read-only and not patched.

Batch dir: `prospective/phase4-exploration/python/http-mongo-neo4j-smtp/`.

## 0. Scope executed

| package | version | levels run (reps) | state |
|---|---|---|---|
| aiohttp | **3.14.5** | B1, B2, B3, M1 × 3 | **complete** (this addendum) |
| curl-cffi | **0.16.3** | B1, B2, B3, M1 × 3 | **complete** (this addendum) |
| pymongo / neo4j | — | — | Covered by the separate [mongo-neo4j batch](../mongo-neo4j/RESULTS-FRAGMENT.md), not this runner |
| httpx / aiosmtplib | 0.28.1 / 5.1.3 | already complete | unchanged, not re-run |

New artefacts: `cases/aiohttp_case.py` (322 lines), `cases/curl_cffi_case.py`
(314), `cases/coverage_probe.py` (129), `run-aiohttp.sh` (83),
`run-curl-cffi.sh` (83), `logs/run.XW86NM/` (aiohttp),
`logs/run.MQwvQx/` (curl-cffi). No existing run directory was overwritten.

## 1. Environment and versions

- Python **3.13.13** (uv-managed) for both targets, as required (not 3.14; the
  engine/`current_task` incompatibility is recorded in `RESULTS-FRAGMENT.md` §1).
- **aiohttp 3.14.5** installed into a **fresh** venv so the shared venv was not
  modified:
  `uv venv -p 3.13 ~/dropwise-py-work-r3/venv-aiohttp313` then
  `uv pip install --python ~/dropwise-py-work-r3/venv-aiohttp313/bin/python 'aiohttp==3.14.5'`.
  Resolved packages: aiohttp 3.14.5, aiohappyeyeballs 2.7.1, aiosignal 1.4.0,
  attrs 26.1.0, frozenlist 1.8.0, idna 3.20, multidict 7.1.0, propcache 0.5.4,
  yarl 1.25.1 (exact list also in `logs/run.XW86NM/environment.txt`).
- **curl-cffi 0.16.3** already present in the shared venv
  `~/dropwise-py-work-r3/venv-py313`; **nothing installed or upgraded** for it.
  Bundled libcurl: `libcurl/8.21.0-IMPERSONATE BoringSSL zlib/1.3.1
  brotli/1.2.0 zstd/1.5.7 libidn2/2.3.7 nghttp2/1.63.0 ngtcp2/1.20.0
  nghttp3/1.15.0` (`logs/run.MQwvQx/environment.txt`).
- Server: the existing `http_server.py`, a separate loopback asyncio process,
  HTTP/1.1 keep-alive, `Content-Length` only, one request at a time per
  connection; ephemeral port; per-connection request log and `/__observe__`.
- Pending: one read-only `DelayProxy` per scenario instance, **0.05 s per
  forwarded chunk in both directions**, in front of the server; the independent
  observer connects directly to the server, bypassing the proxy.
- Bounds: 2.0 s per operation (`asyncio.wait_for`, timeout becomes an `ERR ...`
  value), engine timeout 15.0 s, 3 reps/plan, M1 `Expect.ATOMIC`, pre/post two
  samples per side.
- Runner commands: `./run-aiohttp.sh`, `./run-curl-cffi.sh`. Each starts its own
  server, writes `environment.txt`, per-level stdout/stderr/status and the raw
  server JSONL, then kills the server via an EXIT trap. No container was used.

## 2. Prior exposure (not a blind protocol family)

- **aiohttp**: the package was already studied in `research/python-retro`
  (aiohttp 3.14.0 vs 3.14.1, four levels, all zero, plus an idle-timer
  diagnostic). That work showed aiohttp issue #12795's fix is not on the
  cancellation path. 3.14.5 is a **version extension of a touched target**, not
  a blind one.
- **curl-cffi 0.16.3**: new package, but the HTTP/1.1 client family was already
  touched (aiohttp retro, httpx in this batch). Not a blind protocol family.

## 3. Scenario (both targets)

- **Reuse unit**: one client object per scenario instance with one-connection
  capacity: aiohttp `ClientSession` with `TCPConnector(limit=1,
  limit_per_host=1, force_close=False, enable_cleanup_closed=False)`, HTTP/1.1,
  `auto_decompress=True`, `trust_env=False`; curl-cffi `AsyncSession(
  max_clients=1, timeout=2.0, trust_env=False,
  http_version=CurlHttpVersion.V1_1)`. One warm `GET /warm` establishes the
  keep-alive connection before the target. No reconnect is performed by the
  harness between target and survivor; any reconnect is the client's own and is
  recorded.
- **Target**: `POST /doc/<token>` `{"value": "target", "req": "<token>-target"}`
  through the tested client. This is the state-establishing round trip; its
  effect is the server document and the per-connection request log.
- **B2 `same`**: the same POST with the same arguments (same value, same request
  id) on the same client. **B3 `different`**: `GET /doc/<token>?req=<token>-b3`
  — a different operation of the same kind *on the same document*, so it also
  exposes the target's server-side effect (`value` is `target` if the cancelled
  POST was processed).
- **M1 survivor**: same client, same document: POST value `survivor` with its own
  request id, then GET the same document on the same client (same-connection
  readback of the survivor's own connection). The compared observation is
  protocol/invariant facts only: `target_logged`, `survivor_logged`, `doc_after`
  (observer view), `survivor_post_ok` (status 200 **and** its own request id and
  stored value came back), `readback_ok` (status 200, own read id, value
  `survivor`). Connection ids and timings are kept in DIAG lines only, so a
  legal reconnect does not become an oracle violation.
- **Observer**: a second client (aiohttp `ClientSession`, curl-cffi
  `AsyncSession(max_clients=1)`), direct to the server (no proxy), reads
  `/__observe__?doc=<token>`: document value, the doc-scoped request rows and the
  number of server connections. It bypasses the tested client and the tested
  client's replies.
- **B1/B2/B3** judge as in the engine: B1 only escaping error/timeout after the
  cut; B2/B3 only "the retry is not an error". **M1** judges pre/post with
  `Expect.ATOMIC`; `Neither` is a violation.
- **Contract note**: the installed `ClientSession`/`TCPConnector` docstrings
  contain no cancellation-safety statement (`cancel` absent), and curl-cffi's
  `AsyncSession` docstring only says it may be used as a context manager and
  that this is not required. Public docs could **not** be fetched from this
  environment (`docs.aiohttp.org` and `github.com` both resolve to a non-public
  IP and were refused), so contract statements are taken from installed
  docstrings/source only.

## 4. aiohttp 3.14.5

### 4.1 Levels

`yields = 1` (one realised plan: `pending#1`). Every target run ended
`cancelled/y1/realized=True`; there were **0 baseline errors**.

| level | plans | trials | violations | unknown | unrealized | verdicts | seconds |
|---|---:|---:|---:|---:|---:|---|---:|
| B1 | 1 | 3 | 0 | 0 | 0 | — | 0.66 |
| B2 | 1 | 3 | 0 | 0 | 0 | — | 0.82 |
| B3 | 1 | 3 | 0 | 0 | 0 | — | 0.82 |
| M1 | 1 | 3 | **0** | 0 | 0 | `{'Post': 3}` | 2.55 |

### 4.2 Concrete values

M1, all 3 reps identical after token normalisation:

```
target:    cancelled/y1/realized=True
survivor:  {'target_logged': True, 'survivor_logged': True,
            'doc_after': 'survivor', 'survivor_post_ok': True,
            'readback_ok': True}
observer:  {'doc': 'survivor', 'requests': 3}
verdict:   Post   (Expect.ATOMIC allows Pre/Post; Post = target effect happened)
pre:       {'target_logged': False, ... 'doc_after': 'survivor'}; observer 2 rows
post:      {'target_logged': True,  ... 'doc_after': 'survivor'}; observer 3 rows
```

The pre and post samples were stable (2/2 each), distinguishable (2 vs 3 observed
server rows; `target_logged` False vs True), and no trial was `Unknown`. The
target's POST reached the server before the cut in **3/3** M1 reps, and the
survivor's POST + readback succeeded in **3/3**; the observer confirms the final
document is `survivor` and the target row is present. The readback is on the
survivor's connection (the target's connection is gone), not on the target's
connection — see 4.3.

B2 (3/3): `{'status': 200, 'body': {'conn': <c>, 'req': '<token>-target',
'stored': 'target'}}` — the retry with the same arguments completed.
B3 (3/3): `{'status': 200, 'body': {'conn': <c>, 'req': '<token>-b3',
'value': 'target'}}` — the different operation (GET) completed and also shows
that the cancelled target had already stored `target`.

### 4.3 Acquire-time / pool state and connection reuse

Server-side connection ids (raw `http-server.jsonl`, M1 docs in order):

- uncancelled `count_yields`: target only on conn 31 (no survivor is run);
- pre: survivor POST + read on the **same** conn 33, 35;
- post calibration: target + survivor + read on the **same** conn 37, 39;
- cancelled trials: target on conn 41 / 44 / 47, survivor POST + read on
  **new** conn 43 / 46 / 49.

Connector state at acquire time (private internals, diagnostic only): for the
three cancelled trials the pre-survivor line is
`total=None acquired=0 waiters=0 pooled={}` — the cancelled request's connection
was closed and the connector holds nothing to hand out; `total` is `None`
because aiohttp 3.14.5 has no public total-connection counter. After the
survivor the pooled map holds one idle connected protocol (`idle_s` ~0). The
reconnect is the client's own cleanup, not a harness action: aiohttp
`client.py::_connect_and_send_request` catches `BaseException` around
`resp.start` and does `resp.close(); conn.close()`, and `_request` catches
`BaseException` and closes the response. This is recorded as legal self-healing,
not hidden: with limit=1 the cancelled connection is unusable and a new one is
opened by the survivor.

### 4.4 Triage / upstream

No candidate violation to triage: 12/12 ladder executions passed at the single
realised plan, deterministically across 3 reps, with 0 Unknown and 0 baseline
errors. Source-level cleanup paths (above) are consistent with the observed
state; no engine or client patch was attempted or needed. A web search found the
known related upstream item [aio-libs/aiohttp#12795 "Connection Pool
Poisoning"](https://github.com/aio-libs/aiohttp/issues/12795) — the same issue
the retro studied for 3.14.0/3.14.1 — but the issue and aiohttp docs could not be
fetched (non-public address), so the **current** upstream status is unresolved;
no issue/PR was opened or commented on. Because nothing failed, this is a
negative result at the realised point, **not** "clean" or "safe".

### 4.5 Mid-flight coverage probe (supplementary, not an engine plan)

`cases/coverage_probe.py` starts the target as a plain `asyncio.Task`, polls the
**independent observer** until the server log contains `<token>-target`, then
calls `Task.cancel()` on the still-running target; then runs the same survivor
and observer read.

- 3/3 trials: target request seen before cancel after 0.055/0.056/0.056 s;
  cancel delivered; task outcome `cancelled`; final observer `target_logged:
  True`; survivor POST + readback ok 3/3; final doc `survivor`.
- Server conns: target conn 1/4/7, survivor + read on new conn 3/6/9.
- Result: atomic (`Post` shape) in 3/3. Log:
  `logs/run.XW86NM/coverage-probe.log` + `coverage-probe-server.jsonl`.

## 5. curl-cffi 0.16.3

### 5.1 Levels

`yields = 1`. Every target run ended `cancelled/y1/realized=True`; **0 baseline
errors**.

| level | plans | trials | violations | unknown | unrealized | verdicts | seconds |
|---|---:|---:|---:|---:|---:|---|---:|
| B1 | 1 | 3 | 0 | 0 | 0 | — | 0.52 |
| B2 | 1 | 3 | 0 | 0 | 0 | — | 0.82 |
| B3 | 1 | 3 | 0 | 0 | 0 | — | 0.82 |
| M1 | 1 | 3 | **0** | 0 | 0 | `{'Pre': 3}` | 2.56 |

### 5.2 Concrete values

M1, all 3 reps identical after token normalisation:

```
target:    cancelled/y1/realized=True
survivor:  {'target_logged': False, 'survivor_logged': True,
            'doc_after': 'survivor', 'survivor_post_ok': True,
            'readback_ok': True}
observer:  {'doc': 'survivor', 'requests': 2}
verdict:   Pre    (the cancelled request never reached the server)
pre:       {'target_logged': False, ...}; observer 2 rows
post:      {'target_logged': True,  ...}; observer 3 rows
```

Pre/post were stable (2/2) and distinguishable; no `Unknown`. The survivor's
POST + readback succeeded in **3/3**, but the target was **not** in the server
log in any M1 cancelled trial (`target_logged: False`, observer 2 rows). This is
the important limitation: the engine's cut at the single Python await happens
before libcurl writes the request.

B2 (3/3): `{'status': 200, 'body': {'conn': <c>, 'req': '<token>-target',
'stored': 'target'}}` — retry with the same arguments completed.
B3 (3/3): `{'status': 200, 'body': {'conn': <c>, 'req': '<token>-b3',
'value': None}}` — the GET completed; `value` is `None` because the cancelled
POST never reached the server.

### 5.3 Native-callback coverage limit and pool state

`count_yields` sees **one** suspension: the outer `await` on the future returned
by `AsyncCurl.add_handle`. libcurl performs the transfer in C inside the
AsyncCurl socket callbacks; connection state, request write, header/body
callbacks and completion are not Python awaits and cannot be enumerated. The
ladder therefore has exactly one plan, and at that plan libcurl had not yet sent
the request (observer evidence above). We state this coverage limitation rather
than inserting synthetic cut points.

Acquire-time state: `pool_qsize=1 inflight=0` before every survivor, i.e. the
curl handle was returned to the session's `LifoQueue` and no transfer was
registered in the multi handle. Reuse is only observable through the server:
uncancelled post calibration has target + survivor + read on the **same** conn
31, 33; the cancelled ladder trials have survivor + read on **new** conn 35, 37,
39 with no target row; pre trials use one conn 27, 29. Source: `_request_once`
puts `await task` in a `try/finally` whose `finally` calls `release_curl`;
`release_curl` removes the handle from the multi, resets it and pushes it back,
which explains a usable handle with a dropped connection (survivor reconnects).
Reconnect is the client's own, recorded, not hidden.

### 5.4 Triage / upstream

No candidate violation: 12/12 ladder executions passed at the single realised
plan, 0 Unknown, 0 baseline errors. The M1 verdict `Pre` is allowed by
`Expect.ATOMIC`; it is not evidence that mid-flight cancellation is safe, only
that the ladder's cut is pre-transmission. No matching upstream curl-cffi
cancellation issue was found by search; the only session-management reference
surfaced was a third-party [DeepWiki page on curl_cffi Session and
AsyncSession](https://deepwiki.com/lexiforest/curl_cffi/3.1-session-and-asyncsession-management),
and GitHub/docs could not be fetched (non-public address). Known-vs-new is
**unresolved**; no upstream contact. No patch contrast was attempted (nothing
failed).

### 5.5 Mid-flight coverage probe (supplementary, not an engine plan)

Same observer-triggered probe as 4.5:

- 3/3 trials: target request seen before cancel after 0.051/0.056/0.056 s;
  cancel delivered; task outcome `cancelled`; final observer `target_logged:
  True`; survivor POST + readback ok 3/3; final doc `survivor`.
- Server conns: target conn 10/13/16, survivor + read on new conn 12/15/18.
- Result: atomic (`Post` shape) in 3/3. This **fills the ladder's missing
  regime** — cancel after the server has the request — with a plain C-task
  cancellation. It is still not an engine plan, and libcurl's internal states
  remain unenumerable. Log: `logs/run.MQwvQx/coverage-probe.log` +
  `coverage-probe-server.jsonl`.

## 6. Executions vs independent root causes

| target | ladder executions | probe executions | violations | independent root causes |
|---|---:|---:|---:|---:|
| aiohttp 3.14.5 | 12 (4 levels × 3) | 3 | 0 | 0 |
| curl-cffi 0.16.3 | 12 (4 levels × 3) | 3 | 0 | 0 |
| **total** | **24** | **6** | **0** | **0** |

Execution counts are not root-cause counts. No violation was observed, so no
triage/patch/upstream candidate exists for either target.

## 7. Why these are weak/negative results (coverage, not "clean")

- Both clients expose only **one** Python suspension for this warm-connection
  POST, so the engine realises one plan per level, not a dense cut matrix.
  aiohttp's cut is *after* the request was written (target_logged 3/3); the
  response headers+body arrive in the same proxy-forwarded chunk, so there is no
  body-read suspension. curl-cffi's cut is *before* libcurl writes
  (target_logged 0/3), which is why the probe was added.
- Not covered by either ladder: connect-phase cancellation, request-write
  cancellation, response-body streaming cancellation, connection-close
  cancellation, a second cancelled operation, TLS/HTTP/2, proxy auth,
  redirects, server errors/timeouts, curl-cffi `stream=True`, and any version
  other than the two tested.
- One loopback server, one pending mechanism (50 ms/chunk DelayProxy), one
  server version, 3 reps, no randomised scheduling. No patch contrast was run
  because there was nothing to fix.
- An initial exploratory run had a broken observer call and produced a
  degenerate `pre == post` observation (both sides were the same `TypeError`
  text). It was discarded, the observer call was fixed, and the runs above were
  re-executed in fresh `logs/run.*` directories. The final pre/post are
  distinguishable (2 vs 3 observed rows), so the reported zeros are not a
  calibration artefact.

## 8. Cost and cleanup

Machine time: ladder runs 0.5–2.6 s per level, probes ~1 s; with setup, the two
runners and the probes took about 5 minutes of wall clock. Reading context,
writing the cases and this report brought the whole task to roughly 25–30
minutes. No containers, no external network in the scenario, loopback only.
Both runner scripts kill their `http_server.py` via an EXIT trap; the probes'
servers were killed in the same way. `pgrep` after the runs shows no leftover
`http_server.py`/case/probe processes; `/tmp` scratch and generated
`__pycache__` were removed. No commits, no pushes, no upstream contact, no
changes to `research/python-retro/` or existing run directories.

## 9. Artefact index

- `cases/aiohttp_case.py` — aiohttp 3.14.5 scenario + four-level runner.
- `cases/curl_cffi_case.py` — curl-cffi 0.16.3 scenario + four-level runner.
- `cases/coverage_probe.py` — observer-triggered mid-flight cancel diagnostic.
- `run-aiohttp.sh` / `run-curl-cffi.sh` — servers, environment, levels, cleanup.
- `logs/run.XW86NM/` — aiohttp: `environment.txt`, `level-{B1,B2,B3,M1}.{log,err,status}`,
  `http-server.jsonl`, `http-server.{out,err}`, `http-port`,
  `coverage-probe.{log,err,status,server.jsonl}`.
- `logs/run.MQwvQx/` — the same set for curl-cffi.
