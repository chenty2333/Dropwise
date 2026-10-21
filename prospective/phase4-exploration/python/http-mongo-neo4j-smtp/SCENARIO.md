# Round-3 Python batch: httpx, aiosmtplib, pymongo, neo4j, curl-cffi (unfrozen exploration)

Status: **unfrozen exploration, 2026-10-21.** Not Phase 4 data, not preregistered.
Only httpx and aiosmtplib were run; pymongo, neo4j and curl-cffi are *not
covered* in this report (timebox), see RESULTS-FRAGMENT.md "not covered".

Engine: `research/python-retro/dropwise_py` imported read-only (never modified).
Pending: `research/python-retro/cases/common.py::DelayProxy` (loaded by path, so
this batch's own `cases` package cannot shadow it), 50 ms per forwarded chunk,
both directions. Every target/survivor/observer operation is bounded with
`asyncio.wait_for(..., 2 s)` and the outcome text is kept in the observation.
Engines: B1/B2/B3/M1 exactly as the retro engine; `Expect.ATOMIC` for M1.
`python 3.13.13`; why not 3.14 is recorded in RESULTS-FRAGMENT.md §1.

## A. httpx 0.28.1 (ran, 4 levels × 3 reps)

- **Client/reuse unit:** one `httpx.AsyncClient` with
  `Limits(max_connections=1, max_keepalive_connections=1)`, `http2=False`,
  `trust_env=False`, HTTP/1.1 only. One warm `GET /warm` establishes the
  keep-alive connection before the target.
- **Server:** `http_server.py`, a separate loopback asyncio HTTP/1.1
  keep-alive process; one request at a time per connection, `Content-Length`
  bodies, no pipelining/chunked. It records `(conn id, seq, method, path,
  request id, value)` in a per-connection log and owns the document state.
  Optional `X-Reply-Delay` / `X-Split-Body` are diagnostics only.
- **Pending:** one `DelayProxy` per scenario instance in front of the server.
- **Target:** `POST /doc/<token>` `{"value":"target","req":"<token>-target"}`
  through the tested client. This is the state-establishing round trip; its
  effect is the server's document and request log.
- **Survivor:** same client, same document: `POST` value `"survivor"`, then a
  `GET` readback on the same client (same-connection readback). The compared
  observation is protocol/invariant facts only: `target_logged`,
  `survivor_logged`, `doc_after` (observer view), `survivor_post_ok`
  (status 200 **and** its own request id came back), `readback_ok`.
- **Observer:** a second `AsyncClient`, direct to the server (not through the
  proxy), reads `/__observe__?doc=<token>`: document value, request rows,
  number of server connections. It is independent of the tested pool and of
  the tested client's ability to read its own replies.
- **Contract:** no cancellation-safety promise was found in the httpx client
  documentation; connection reuse after an unread response is not documented
  as a caller obligation either. Classification is therefore stated as a
  client cleanup defect with an unresolved upstream/contract status, not as a
  documented "guaranteed" violation.
- **Result:** B1 0/18; B2 3/18; B3 3/18; M1 3/18; all violations `pending#6`.
  See RESULTS-FRAGMENT.md §3.

## E. aiosmtplib 5.1.3 (ran, 4 levels × 3 reps)

- **Client/reuse unit:** one long-lived `aiosmtplib.SMTP` object, connected once
  in setup; **no reconnect** between target and survivor. One warm `NOOP`.
- **Server:** GreenMail 2.1.14 (`greenmail/standalone:2.1.14`) SMTP 3025 and
  REST 8080, loopback only, fresh GreenMail user per scenario instance
  (`POST /api/user`).
- **Pending:** one `DelayProxy` per scenario in front of SMTP 3025.
- **Target:** one complete `SMTP.send_message` of a message with subject
  `<token> target`, Message-ID `<token>.target@dw.test>` (MAIL/RCPT/DATA/final
  250).
- **Survivor:** a second `send_message` on the *same* SMTP object, subject
  `<token> survivor`, then `NOOP` on the same object as same-connection
  readback. Compared observation: `send` concrete reply codes, `noop` code,
  mailbox subject list, recipient count.
- **Observer:** GreenMail REST `GET /api/user/<user>/messages`, direct to 8080
  (bypasses the SMTP object and the proxy). Subjects are normalised
  `<token>` → `<run>`.
- **Contract:** aiosmtplib documents `send_message` raising
  `SMTPDataError` "on unexpected server response code" and `SMTPServerDisconnected`
  on connection loss; it does not document cancellation as closing or
  realigning the connection. Contract status: no cancellation-safety statement
  found; the observed error is the documented error type, but for a reply that
  belongs to the cancelled send.
- **Result:** B1 0/12; B2 **12/12**, B3 **12/12**, M1 **12/12** (all `Neither`
  under `Expect.ATOMIC`; pre/post stable). See RESULTS-FRAGMENT.md §4.

## C. pymongo 4.19.0 (not covered)

Planned shape, not run: `AsyncMongoClient(maxPoolSize=1)` against a local
`mongo:7.0.21` single-node replica set (`rs.initiate`, unique db/collection);
target = `ClientSession.start_transaction()` + insert of the target document;
survivor = same pool, same `_id`, side effect + same-connection readback;
observer = independent `AsyncMongoClient` plus `currentOp`/document read.

**A `ClientSession` is not a physical connection handle.** With
`maxPoolSize=1` the session pins a connection only when an operation runs, and
the session object outlives individual checked-out connections; target and
survivor share the *pool* and the *document*, not a client-held socket. Any
cancellation result must therefore be reported as pool/session state, not as
"same handle".

## D. neo4j 6.4.1 (not covered)

Planned shape, not run: `AsyncGraphDatabase.driver(..., max_connection_pool_size=1)`
against `neo4j:5.26.0-community`; target = `begin_transaction` + write of a
node with a unique tag; survivor = same driver pool, same tag, side effect +
same-connection readback; observer = independent async driver.

## B. curl-cffi 0.16.3 (not covered)

Planned shape, not run: `AsyncSession(max_clients=1)`, force HTTP/1.1 via
`http_version=CurlHttpVersion.V1_1`, same server/proxy shape as httpx.
Native-callback coverage limits to record when it is run: the asyncio yield
count only sees the Python await around libcurl; libcurl's transfer continues
in its own thread and its state transitions are not enumerable, so
`unrealized`/`Unknown` attribution must be reported separately from httpx.
