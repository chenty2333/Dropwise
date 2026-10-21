# async-pop 1.1.3 — direct `Client<S>` cancellation reuse (round 3)

## Family and versions

- Client: `async-pop = "=1.1.3"`, `default-features = false,
  features = ["runtime-tokio"]`; direct `async_pop::Client<tokio::net::TcpStream>`
  created with `async_pop::new(stream)`. No pool/manager.
- Service: GreenMail 2.1.14 was present and probed, but **not used for the
  ladder**: `async_pop::new` + `login` hangs on GreenMail's chunked CAPA reply
  (see RESULTS-FRAGMENT, "GreenMail blocker"). The allowed fallback is used:
  `pop_server.py`, a loopback asyncio POP3 server implementing real
  USER/PASS/CAPA/STAT/RETR/RSET/NOOP/QUIT with one seeded message, plus a
  separate HTTP state endpoint on a second loopback port.
- Pending: `dropwise::net::Proxy` with `reply_delay = 50 ms` on the tested
  connection only; the Python server replies at once (no server-side delay).
- Observer: the server's HTTP `/state` endpoint over its own TCP connection
  (never through the proxy and never through the tested handle). It reports the
  server-owned mailbox: `(message count, subject present)`.
- Tokio 1.54.0, `Flavor::MultiThread { workers: 1 }`.

## Operations and reuse unit

Setup connects through the proxy and calls `async_pop::new`, which reads the
greeting and CAPA. That `Client` is held outside every target future and is
never reconnected before the survivor; `Proxy::connections()` stays 1.

- Target 1: `login(user, pass)` — the session-establishing exchange (USER,
  PASS, and CAPA again after PASS; several awaits).
- Target 2: if `login` completed, `retr(1)` on the same handle is marked as a
  second target. With `max_cancellations = 1` the search cuts one of the two.
- B2: retry whichever target was cancelled, same arguments, same handle
  (`login` again, or `retr(1)` again).
- B3: a different command, `capa()`, on the same handle (valid in both
  AUTHORIZATION and TRANSACTION state).
- M1 survivor: `stat()` then `retr(1)` on the same handle.
- Observer before/visible: HTTP `/state` → `(1, true)` in every baseline.

## Observation (M1)

```text
before  = observer (messages, subject)       ((1,true) in all samples)
stat    = same-handle STAT                   "1/159" or Err(text)
retr    = same-handle RETR 1                 (157, true) or Err(text)
visible = observer (messages, subject)       ((1,true))
```

`Expect::Atomic`. Calibration for a login cut: `pre` drops login before its
first poll, so the session is never authenticated — `stat`/`retr` correctly get
the server's `-ERR not authenticated` and the client maps it to `Err("Server
error")`. Both pre samples are identical, both post samples are `stat="1/159",
retr=(157,true)`, and pre != post. For a retr cut, `pre` and `post` both keep
the login and differ only by the skipped RETR, so both sides are `Ok("1/159")`
and `(157,true)`: the retr-target M1 has a clean successful calibration on both
sides.

## Contract read first

Public docs list POP3 commands and client states but say nothing about
dropping a command future, reusing the connection afterwards, or resetting the
stream. There is no public cancel/reset/drain API. A cancelled command's
already-delivered response therefore remains in the client's buffer; the next
command can read it. Judgement is from the concrete next-command result and the
server-owned mailbox observer, not from `Ok`.

## Pending and files

- `src/main.rs`: setup, preflight, manual calibration, B1/B2/B3/M1.
- `pop_server.py`: real-protocol POP3 server, one seeded message, JSONL
  command log, independent HTTP `/state` observer.
- `run-ladder.sh`: ephemeral loopback ports, fresh `logs/run.XXXXXX/`,
  process cleanup trap.
- Raw logs: `logs/run.WKXVqY/` (valid), `logs/run.7q9n0j/` (invalid, GreenMail
  attempt; kept, not overwritten).

## Recovery contrast

No reset API. The practical contrasts are `rset` (only re-marks deleted
messages; it does not drain an unread reply) and drop + fresh connection. They
are recorded as coverage notes, not used as the survivor.
