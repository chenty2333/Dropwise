# async-pop 1.1.3 — results fragment (round 3, unfrozen exploration)

Not Phase 4 data. One target (two marked exchanges on one handle). No commit,
no upstream contact.

## Versions / environment

| item | value |
|---|---|
| client | `async-pop` 1.1.3, `runtime-tokio`, direct `Client<tokio TcpStream>` |
| tokio | 1.54.0 |
| dropwise | path `../../../../../..` |
| service | `pop_server.py`, python 3.14.8 asyncio POP3 + HTTP observer |
| service ports | 127.0.0.1:43223 (POP3), 127.0.0.1:37861 (observer), ephemeral |
| seeded message | 1, subject `dw-r3-pop-seed`, 159 octets |
| proxy | `dropwise::net::Proxy`, `reply_delay = 50 ms` |
| config | MultiThread(1), races Immediate+AfterWake, max_cancellations=1, max_runs=64, scenario_timeout=20 s, settle 0/0 watchdog 10 s |
| valid run | `logs/run.WKXVqY/` |
| invalid run | `logs/run.7q9n0j/` (GreenMail attempt; kept, marked invalid) |

## GreenMail blocker (why the fallback)

GreenMail 2.1.14 (`sha256:1ef95a9664…`, POP3 on a loopback port) greets, accepts
`USER popuser@example.com` / `PASS pass` and serves `STAT`/`RETR`; a message
seeded with Python `smtplib` is visible both over POP3 and at
`GET /api/user/popuser@example.com/messages` (the planned independent REST
observer). But `async_pop::new` + `login` hangs against it even without the
proxy: the crate's `PopStream::decode` (`src/stream.rs`) sets `decode_needs`
on `nom::Err::Incomplete` and does **not** reset it in the success branch, so
after GreenMail's CAPA reply arrives in two TCP segments the following short
`+OK` response is never considered complete. This is a client bug independent
of cancellation; the ladder therefore uses the brief's allowed loopback POP3
fallback. It also means the GreenMail REST observer is not part of the valid
run; the server's own HTTP state endpoint is used instead.

## Preflight (valid run)

```text
preflight_setup proxy_connections=1 (client 36804, 6/45 bytes)
login=Ok(()) retr_len=157 stat=1/159 second_len=157
before=(1,true) visible=(1,true) proxy_connections=1
PREFLIGHT-OK same_handle=reused observer_unchanged=true
```

## Calibration (2 samples per side, manual, identical per side)

| side | stat | retr | observer |
|---|---|---|---|
| pre ×2 | `Err("Server error")` (server `-ERR not authenticated`) | `Err("Server error")` | `(1,true)` |
| post ×2 | `Ok("1/159")` | `Ok((157,true))` | `(1,true)` |

For a login cut this `pre` is the honest "login never started" state: the
survivor's STAT/RETR reach the server and are answered `-ERR not
authenticated` (verified in `pop-commands.jsonl`). It is stable, not a
calibration failure. For a retr cut, both sides are the successful post state.

## Ladder (3 repetitions, identical counts)

| level | plans | violations | unrealized | unsettled | baseline_errors | exhaustive |
|---|---:|---:|---:|---:|---:|---|
| B1 LOGIN_RETR | 8 | 0 | 0 | 0 | 0 | true |
| B2 LOGIN_RETR | 8 | 2 | 0 | 0 | 0 | true |
| B3 CAPA | 8 | 6 | 0 | 0 | 0 | true |
| M1 LOGIN_RETR | 8 | 8 raw Neither (2 unambiguous failures, 6 unresolved) | 0 | 0 | 0 | true |

`baseline_targets=2`; plans are `#0@1..#0@3` (login) and `#1@1..#1@3` (retr)
× {Immediate, AfterWake}. M1 verdicts: neither=8, pre=0, post=0, mixed=0,
unknown=0.

### Unambiguous failure: retr target with successful pre == post

Plan `#1@1` (both races, 3/3 reps), calibration `pre = post = {stat:Ok("1/159"),
retr:Ok((157,true))}`. The cancelled RETR's multi-line reply is still in the
client when the survivor starts:

```text
observed stat = Err("Did not received the expected stat response")
observed retr = Err("Did not received the expected retr response")
server log    = RETR:+OK 159 octets | STAT:+OK | RETR:+OK 159 octets
observer      = (1,true) before and after
```

The server answered the survivor's STAT and RETR correctly, but the client
consumed the old RETR body as the STAT reply, then the STAT reply as the RETR
reply. This is old-vs-new confusion with both calibration sides successful;
it is counted as a violation.

### Login-target M1 (6/6 plans): partial state, reported unresolved

For cuts of `login` (#0@1..#0@3) pre is the unauthenticated `Err("Server
error")` side and post is `Ok("1/159")`. The observed state is neither:

```text
plan #0@1/#0@2 examples (3/3 reps):
  observed stat = Err("Did not received the expected stat response")
  observed retr = Err("Server error")
  server log    = USER:+OK | STAT:-ERR not authenticated | RETR:-ERR not authenticated
```

The server received and answered the survivor's STAT (`-ERR`), but the client's
STAT consumed the old `USER +OK`; RETR then consumed STAT's `-ERR`. So the
server-visible exchange equals `pre` while the client-visible observation
differs from both sides. Under `Expect::Atomic` that is a partial state, but
the permitted partial-authentication state and recovery responsibility have not
been established, these 6 plans remain **unresolved**, not confirmed defects.
A stable error-valued pre endpoint is not by itself invalid calibration.

### B2 (2/8 plans) — no calibration involved

Plan `#0@3` (login cut at its third pending boundary), both races, 3/3 reps:
retry `login` on the same handle fails with
`Err("Did not received the expected user response")`. Server log shows the
cancelled login's USER/PASS/CAPA were served, then the retry's USER; the retry
read the stale CAPA reply instead of its USER reply. This is a concrete
old-vs-new failure of the *same* operation with the same arguments.

### B3 (6/8 plans) — no calibration involved

Plans `#0@1`, `#0@2`, `#1@1`, both races, 3/3 reps: `capa()` on the same handle
fails with `Err("Did not received the expected capa response")`. The server log
shows CAPA was sent and answered; the client read the preceding stale reply
(e.g. `USER +OK`, `PASS +OK`, or the RETR body). CAPA is legal in both states,
so this is not a legitimate server refusal.

## Old vs new, explicitly

| trial | server actually sent/answered | client returned |
|---|---|---|
| pre calibration | `STAT -ERR not authenticated`, `RETR -ERR` | `Err("Server error")` both (matches server) |
| cancelled login #0@1/#0@2 | `USER +OK`, `STAT -ERR`, `RETR -ERR` | `stat=Err("Did not received the expected stat response")`, `retr=Err("Server error")` |
| cancelled retr #1@1 | `RETR +OK`, `STAT +OK`, `RETR +OK` | both `Err("Did not received the expected … response")` |
| B2 #0@3 | login reply then retry USER | `Err("Did not received the expected user response")` |

The server log and the independent observer agree that the mailbox was never
modified (`(1,true)` throughout); the damage is in the client's response
association, not in mailbox state.

## Triage

1. Harness exclusion: B1 baseline and B2/B3 baselines are clean
   (`baseline_errors=0`); preflight shows a working handle and observer; the
   server log independently records the commands and answers. `run.7q9n0j` is
   invalid: `rest_count` bug plus an unbounded `login` in the manual
   calibration hung it; it is kept but not used.
2. Source trace: `send_request` → `PopStream::encode` → `read_response`; there
   is no drain on future drop and no reset API. `decode_needs` is not reset on
   success (the same mechanism that hangs GreenMail login), which can extend a
   desync beyond a single stale reply.
3. Classification: **confirmed client-side response-association candidate**.
   The retr-target M1 (clean pre/post) and B2/B3 (no calibration) are concrete;
   the login-target M1 remains unresolved pending its partial-authentication
   contract; an error-valued pre endpoint alone does not invalidate it. Not memory-safety.
4. Upstream: read-only search of the Dust-Mail/async-pop repository/issues
   found no matching cancellation/reuse or CAPA-chunking report. Status: **new
   to this exploration, upstream unknown**.
5. Patch contrast: not attempted. A minimal fix would drain/reset the response
   buffer before reusing a handle after cancellation (and reset `decode_needs`
   after success); neither is exposed by the public API, so a dependency-copy
   patch was out of the timebox. Recorded as not covered.
6. Recovery contrast (`rset`, fresh connection) was not exercised; `rset` does
   not drain a pending reply, so it is not claimed as a fix.

## What this does not show

- Only one handle, one mailbox, one message; POP3 only, no TLS/SASL/TLS.
- GreenMail itself was not used for the ladder because of the CAPA-chunking
  hang; the REST observer was validated in the precheck only.
- Two marked targets, up to 3 pending boundaries each, 2 races; plans bounded
  by `max_runs=64`.
- No claim about versions other than 1.1.3, or about APIs that reset the
  stream.
