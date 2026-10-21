# RESULTS-FRAGMENT: async-imap 0.12.0, direct handle, cancelled SELECT INBOX

Status: **unfrozen Phase 4 exploration, round 1**. One client version, one
server, one operation. This is not the frozen Phase 4 study and not a proof.
Generated 2026-10-21 from the clean runner in this directory.

## Bottom line

The TCP connection survives a cancelled `SELECT INBOX`; the logical session
does not. After the SELECT future is dropped at an observed `Pending`, the very
next operation on the same `Session` sees responses left by the abandoned
SELECT:

- B3: `Session::append("INBOX", ...)` returns
  `Err(Error::Append)` ("could not append mail to mailbox"), the message is not
  stored (independent REST count 0), and the session is left waiting for the
  APPEND literal.
- M1: APPEND returns `Err(Error::Append)`; the following SEARCH times out at
  the 5 s per-step bound, and FETCH is never sent (it inherits the search
  error); the independent REST observer sees only the seed message. Observed value is neither the pre nor the post calibration
  (12/12 realized cancellations, 3 repetitions).
- B2: re-issuing the same SELECT returns `Ok`, but at a first-`Pending` cut the
  returned `Mailbox` has every flag duplicated, because it merged the abandoned
  SELECT's untagged FLAGS with the new SELECT's FLAGS. At a second-`Pending`
  cut the flags were clean. B2 passes its "only Ok" rule, but the Ok value is
  not a clean SELECT result.
- B1: no panic, liveness failure or obligation leak in 12 trials (4 plans x 3
  repetitions); one `#0@2:AfterWake` plan was unrealized in rep 3.

The failure is not a server protocol violation. GreenMail correctly completes
the SELECT and correctly sends the APPEND continuation (`+`); the client reads
one stale SELECT response instead of the continuation and gives up. The
server-side log independently matches every failed APPEND (22
`Command should be '<tag> APPEND ... literal'` warnings = 1 probe + 9 B3 + 12
M1). A 7-line dependency-copy change to the APPEND continuation read removes
the B3/M1 failures, but not the B2 flag duplication.

Contract status: **no general cancellation contract**. async-imap 0.12.0's
README and API docs do not state whether dropping an arbitrary command future
is supported or what recovery is required. The 0.12.0 changelog does document
one partial precedent: LOGIN now ignores a `NO` completion for another request,
and PR #142 explicitly mentions a dropped response-reading future. That fix is
LOGIN-specific; APPEND still reads exactly one response. This is therefore a
confirmed user-visible consequence with an unspecified general contract, not a
violation of a written cancellation promise. If cancellation is meant to be
supported generally, this is a library bug; if not, the docs need an explicit
"not cancel-safe" contract and a recovery API.

## Environment

From `environment.txt` and `logs/container.txt`:

| item | value |
|---|---|
| client | `async-imap 0.12.0`, `default-features = false`, feature `runtime-tokio` |
| async-imap deps | `imap-proto 0.17.0`, `tokio 1.54.0`, `futures-util 0.3.34`, `serde_json 1.0.154` |
| Dropwise | path `../../../..`, package `dropwise 0.1.0`, feature `net` |
| toolchain | `rustc 1.98.0 (88d9e12ae 2026-08-18)`, `cargo 1.98.0 (797e8a9bc 2026-08-05)` |
| OS / Docker | `Linux 7.2.11-cachyos1.fc44.x86_64`, Docker `29.8.2` |
| server | `greenmail/standalone:2.1.14` |
| image id | `sha256:1ef95a966418cd09b7ea91d504d8c0826bbe7a2f6e679a75c601a831587c1626` |
| repo digest | `greenmail/standalone@sha256:1ef95a966418cd09b7ea91d504d8c0826bbe7a2f6e679a75c601a831587c1626` |
| GreenMail line | `Starting GreenMail standalone v2.1.14` |
| ports in clean run | SMTP `127.0.0.1:32784`, IMAP `127.0.0.1:32785`, REST `127.0.0.1:32786` |
| runtime | `Flavor::MultiThread { workers: 1 }`, 50 ms proxy `reply_delay`, `Settle { tokio_time: 0, real_time: 0 }` |

`Settle::ZERO` does not exist; the effective zero settle above is what the task
intended.

## Commands

Clean runner (one disposable container, one preflight, one probe, three ladder
processes):

```sh
cd prospective/phase4-exploration/clients/imap
bash run.sh
```

The container command inside `run.sh`:

```sh
docker run -d --rm --name dw-p4-imap-$$ \
  -p 127.0.0.1::3025 -p 127.0.0.1::3143 -p 127.0.0.1::8080 \
  -e GREENMAIL_OPTS='-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0 -Dgreenmail.auth.disabled -Dgreenmail.users=imapuser:pass@example.com' \
  greenmail/standalone:2.1.14
```

Individual phases (same environment variables `P4_SMTP_PORT`, `P4_IMAP_PORT`,
`P4_REST_PORT`):

```sh
./target/release/imap-explore preflight
./target/release/imap-explore probe
./target/release/imap-explore ladder 1   # then 2, 3
```

Raw logs: `logs/preflight.stdout.log`, `logs/preflight.stderr.log`,
`logs/probe.stdout.log`, `logs/probe.stderr.log`, `logs/round-1..3.*`,
`logs/greenmail.log`, `logs/timings.txt`.

## Preflight evidence (before any ladder interpretation)

### 1. No-cancel run

From `logs/preflight.stdout.log`:

```text
NOCANCEL select=Ok("exists=1 recent=1 unseen=Some(1) uid_next=Some(2) uid_validity=Some(1792519514) flags=[Answered, Deleted, Draft, Flagged, Seen]") raw="Ok" append=Ok(()) raw="Ok"
NOCANCEL search=Ok(1) raw="seqs=[2]" fetch=Ok("p4-imap-survivor-body") raw="seq=2 text_bytes=21"
NOCANCEL observer=Ok(ObserverView { run_subjects: ["s-RUN-seed", "s-RUN-survivor"], run_message_count: 2, seed_body: true, survivor_body: true })
NOCANCEL proxy_after=client=127.0.0.1:43406 up=433 down=558
```

The observer is direct REST; it saw the SMTP seed and the appended message.
The mail path works end-to-end with the 50 ms proxy.

### 2. Reuse / identity diagnostic

From `logs/preflight.stdout.log`:

```text
REUSE start ... session_ptr=0x556baf1c6d10 stream_ptr=0x556baf1c6d10 conns=1 stats=[client=127.0.0.1:54548 up=31 down=74]
REUSE after_cmd1 select=Ok("exists=3 recent=1 ...") raw="Ok" conns=1 stats=[client=127.0.0.1:54548 up=53 down=356]
REUSE after_cmd2 append=Ok(()) raw="Ok" conns=1 stats=[client=127.0.0.1:54548 up=345 down=439]
REUSE after_cmd3 search=Ok(1) raw="seqs=[4]" conns=1 stats=[client=127.0.0.1:54548 up=393 down=479]
REUSE end ... same_session=true same_stream=true conns=1
```

Three sequential commands used one accepted TCP connection, one proxy
connection entry, the same boxed `Session` pointer and the same stream pointer.
This is the positive control for "same object, same connection".

### 3. Manual pre/post calibration

From `logs/preflight.stdout.log`:

```text
CALIBRATION side=pre select=None append=Ok(()) search=Ok(0) fetch=Err("NoMatch") observer=Ok(ObserverView { run_subjects: ["s-RUN-seed", "s-RUN-survivor"], run_message_count: 2, seed_body: true, survivor_body: true })
CALIBRATION side=post select=Some(Ok("exists=7 recent=3 ...")) append=Ok(()) search=Ok(1) fetch=Ok("p4-imap-survivor-body") observer=Ok(... same two subjects ...)
```

Both sides complete and the observer sees the effect in both. The pre side's
`search = Ok(0)` is not a genuine zero-match result: GreenMail's log shows
"Command 'SEARCH' not valid in this state AUTHENTICATED" for every unselected
SEARCH, but async-imap returned Ok(empty) (see adjacent finding below).

### 4. Triage probe (public API only; added after the first run)

`logs/probe.stdout.log`:

- **A**: drop SELECT via `tokio::time::timeout(40 ms, ...)`; after 150 ms the
  proxy has delivered the abandoned reply. APPEND returns
  `Err("Append")` / "could not append mail to mailbox". A manual
  `read_response()` walk then sees, in order:
  `MailboxData(Exists(9))`, `MailboxData(Recent(1))`, UIDVALIDITY, UIDNEXT,
  UNSEEN, PERMANENTFLAGS, `Done { tag: RequestId("A0002"), status: Ok, ... SELECT completed. }`,
  then `Continue { information: Some("OK") }` — the APPEND `+`.
  The independent observer sees 0 messages for the probe subject. The first
  stale response (FLAGS) was consumed by APPEND; the `+` is still queued behind
  the rest.
- **B**: drop SELECT, drain the queued responses with `read_response()` until
  the SELECT tagged Done (8 responses), then APPEND returns `Ok(())` and the
  observer sees 1 message.

This is a caller-side contrast, not a library fix: draining the abandoned reply
before APPEND restores the append. It confirms the queued-response mechanism
without reading any async-imap source.

## Ladder results

All levels use the same setup/target/Pending/Config. `plans` is
`Report.trials.len()` (one per generated cancellation plan; the baseline is not
a trial). `baseline_errors` and `unsettled` were 0 in every row;
`exhaustive=true` and `baseline_targets=1` in every row.

| level | rep | plans | violations | unrealized | baseline_errors | actual / verdict |
|---|---:|---:|---:|---:|---:|---|
| B1 | 1 | 4 | 0 | 0 | 0 | no panic/timeout/leak; all cuts realized |
| B1 | 2 | 4 | 0 | 0 | 0 | no panic/timeout/leak; all cuts realized |
| B1 | 3 | 4 | 0 | 1 | 0 | `#0@2:AfterWake` unrealized (target completed before the 2nd Pending) |
| B2 | 1 | 4 | 0 | 0 | 0 | 4/4 retry SELECT `Ok`; `@1` flags duplicated (10), `@2` flags clean (5) |
| B2 | 2 | 4 | 0 | 0 | 0 | same pattern; all 4 `Ok`, `@1` duplicated, `@2` clean |
| B2 | 3 | 4 | 0 | 0 | 0 | same pattern; all 4 `Ok`, `@1` duplicated, `@2` clean |
| B3 | 1 | 4 | 3 | 1 | 0 | 3 realized cuts `Err(Error::Append)`, observer 0; `#0@2:Immediate` unrealized |
| B3 | 2 | 4 | 4 | 0 | 0 | 4/4 `Err(Error::Append)`, observer 0 |
| B3 | 3 | 2 | 2 | 0 | 0 | only one Pending observed, so only 2 plans; 2/2 `Err(Error::Append)`, observer 0 |
| M1 | 1 | 4 | 4 | 0 | 0 | 4/4 Neither; `pre=0 post=0 mixed=0 neither=4 unknown=0` |
| M1 | 2 | 4 | 4 | 0 | 0 | 4/4 Neither; same counts |
| M1 | 3 | 4 | 4 | 0 | 0 | 4/4 Neither; same counts |

Totals: 46 planned trials; 21 violations (B3 9 + M1 12); 2 unrealized
(B1 1, B3 1); no baseline errors; no unsettled runs; all reports had
`exhaustive=true` within the generated plans.

### B2 = 0 and B3 failures are not contradictory

B2's preregistered rule is only "retry SELECT returns `Ok`". It stays at **0
violations** in every repetition, and that number must not be replaced by the
B3/M1 result. The contrast is real and is the most interesting level result:

- **B2 (SELECT retry)** parses full mailbox responses, consumes and skips a
  stale tagged Done that is not its own, and reaches *its own* SELECT Done; it
  returns `Ok`. The call succeeds even though the returned `Mailbox` merged
  stale untagged FLAGS (10 flags at `@1`).
- **B3 (APPEND next)** does not parse a mailbox; it expects exactly one
  `Response::Continue` (`+`) as the next response. The next response is stale
  SELECT untagged data, so it returns `Err(Error::Append)` before writing the
  literal.

So SELECT can resynchronize far enough to return `Ok` while the session is
still unusable for APPEND. B2's zero is a fact about its Ok-only criterion, not
evidence that the session is healthy. The actual B2 values below are recorded
separately and are not clean (`@1` duplicate flags).

### Actual B2 values (representative; all repetitions match)

At `#0@1:Immediate` / `#0@1:AfterWake` (rep 1):

```text
retry = Ok(Mailbox {
  exists=18, recent=0, unseen=Some(1), uid_next=Some(19),
  uid_validity=Some(1792519514),
  flags=[Answered, Deleted, Draft, Flagged, Seen,
         Answered, Deleted, Draft, Flagged, Seen]
})
```

At `#0@2:Immediate` / `#0@2:AfterWake` (rep 1):

```text
retry = Ok(Mailbox {
  exists=20, recent=0, unseen=Some(1), uid_next=Some(21),
  uid_validity=Some(1792519514),
  flags=[Answered, Deleted, Draft, Flagged, Seen]
})
```

The absolute `exists`/`uid_next` numbers differ by repetition because the
mailbox grows; the flag pattern is stable: **6/6 first-Pending retries have ten
flags, 6/6 second-Pending retries have five flags** across the three
repetitions. B2 only judged `Ok`, so this contamination is not counted as a
B2 violation, but it is visible in the recorded value.

### Actual B3 values

Every realized cancellation (9/9) recorded:

```text
append = Err(Error::Append)
raw    = "could not append mail to mailbox"
REST   = Ok(0) matching messages (e.g. rest_total=23, subject="s-p4imap-...-b3", count=0)
```

The append was not stored. The server log then reports an incomplete APPEND
literal. There is no case of "Ok but observer 0" in the unpatched run.

### Actual M1 observations

One representative trial (all 12 realized cancellations were equal):

```text
Obs {
  append: Err("Append"),
  search: Err("Timeout"),
  fetch_text: Err("Timeout"),
  observer_subjects: Ok(["s-RUN-seed"]),
  observer_seed_body: true,
  observer_survivor_body: false,
  observer_run_message_count: 1,
}
```

Stable calibrations (two samples each side, all three repetitions):

```text
pre  = Obs {
  append: Ok(()),
  search: Ok(0),
  fetch_text: Err("NoMatch"),
  observer_subjects: Ok(["s-RUN-seed", "s-RUN-survivor"]),
  observer_seed_body: true, observer_survivor_body: true,
  observer_run_message_count: 2,
}
post = Obs {
  append: Ok(()),
  search: Ok(1),
  fetch_text: Ok("p4-imap-survivor-body"),
  observer_subjects: Ok(["s-RUN-seed", "s-RUN-survivor"]),
  observer_seed_body: true, observer_survivor_body: true,
  observer_run_message_count: 2,
}
```

So the trial is neither "operation never started" (which still lets APPEND work
and the observer see the survivor) nor "operation completed" (which lets APPEND
and SEARCH work). The cancelled trial loses the survivor's effect entirely and
then wedges the next command. `unknown=0`: the calibrations were stable, not
ambiguous.

## Violation details and triage

### Mechanism

In async-imap 0.12.0:

- `Session::select` (`src/client.rs:380`) sends SELECT with tag A0002 and
  `parse_mailbox` (`src/parse.rs:227`) reads the untagged data and the matching
  tagged Done. Dropping the future at a Pending leaves those already-sent or
  in-flight responses in the socket.
- `Session::append` (`src/client.rs:1160`) sends APPEND and then reads **one**
  response, expecting `Response::Continue` (the `+`). Any other response is
  turned into `Error::Append` immediately (`src/client.rs:1180-1185`). It does
  not skip responses for other tags, unlike `check_done_ok_from`
  (`src/client.rs:1462`), which already loops past non-matching tagged Done
  responses.
- The abandoned SELECT's untagged data is the first response in the queue. In
  probe A APPEND consumed the FLAGS response; the SELECT tagged Done and the
  APPEND `+` remained queued. The client had already sent APPEND; the server
  parsed it and sent `+`, but the client never wrote the literal content and
  returned `Error::Append`.
- The following SEARCH is written while the server is still waiting for the
  APPEND literal. The server consumes it as literal data, so it never sends a
  tagged SEARCH response; the client waits until the 5 s per-step bound. This is
  why M1 SEARCH is Timeout rather than Bad/No. FETCH is not attempted at all;
  its `fetch_text = Err("Timeout")` is the propagated search error, not an
  independent fetch timeout (the raw log says "not attempted: search failed").
- `parse_mailbox` ignores a tagged Done with a different tag but processes all
  untagged data. A retry SELECT therefore merges the abandoned SELECT's
  `FLAGS` into its `Mailbox`, producing the duplicated flags at a
  first-Pending cut. At a second-Pending cut the retry's flags were clean; the
  most consistent explanation is that the cancelled future had already
  consumed FLAGS (probe B shows FLAGS is the first SELECT response), so the
  retry saw only the remaining untagged data. This is an inference from the
  observed 6/6 vs 6/6 pattern, not a direct trace of the cancelled future.

No async-imap implementation was read before the first cancellation run. The
source was read only after the B3/M1 results appeared, as triage.

### Independent server evidence

From `logs/greenmail.log` of the clean run:

- `22` occurrences of
  `Error processing command: Unexpected end of stream. Command should be '<tag> APPEND <mailbox> [<flag_list>] [<date_time>] literal'`.
  The accounting matches exactly: 1 probe-A failed APPEND + 9 B3 failures + 12
  M1 failures = 22.
- `7` occurrences of
  `Command 'SEARCH' not valid in this state AUTHENTICATED`: 1 preflight pre
  calibration + 6 M1 pre samples (2 x 3). This is the adjacent search finding,
  not a cancellation failure.
- `3` broken-pipe lines, consistent with dropping sessions mid-literal at the
  end of runs; no server crash.

This is an independent record (server side) that every failed APPEND was
received and left incomplete, and that the cancelled SELECT itself completed on
the server. It is a server-side count, not a client-side inference.

### Minimal dependency-copy patch contrast

Outside the repository, `/tmp/async-imap-patched` is a copy of async-imap
0.12.0; the experiment crate was copied to `/tmp/p4-imap-patched` with
`[patch.crates-io]`. The only source change is the APPEND continuation read:
skip non-`Continue` responses until the server's `+`, and stop on this
command's own tagged Done. Diff and raw output:
`logs/patched-async-imap.diff`, `logs/patch-contrast.txt`.

Results (triage only, one probe and one ladder round, not the official runs):

- patched probe A: APPEND `Ok(())`, observer 1 (unpatched: `Err(Append)`,
  observer 0).
- patched ladder 1: B3 `plans=4 violations=0 unrealized=0`; 4/4
  `append=Ok(()) observer=1`. M1 `plans=4 violations=0`; 4/4 verdict Post.
- patched B2 still returns duplicated flags at `@1`: the patch does not touch
  `parse_mailbox`/`select`. This isolates the append failure from the B2
  metadata issue.

The patch is not validated against the upstream test suite and silently
discards stale responses, so it is evidence of a removable cause, not a
recommended final fix.

### Contract reasoning and classification

- Public docs, README and CHANGELOG do not mention cancellation, dropping a
  command future, or a required resynchronization. The API presents
  `Session` as a long-lived handle with methods taking `&mut self`; dropping a
  future releases that borrow, which is exactly the operation B3/M1 perform.
- `Error::Append` is misleading: it reads like a server rejection of APPEND,
  while the server actually accepted APPEND and is waiting for the literal.
  A caller's natural retry could append twice or hang again; there is no
  documented `drain`/reset API.
- IMAP itself is not at fault: the server's SELECT responses and APPEND
  continuation are valid and ordered. The client fails to correlate/skip the
  abandoned responses.
- Classification: **confirmed user-visible consequence, undocumented general
  contract, with a partial upstream precedent**. This is a credible library bug:
  the project has already fixed the same stale-response-after-dropped-future
  class for LOGIN (PR #142) and the patch contrast shows the APPEND consequence
  is removable, but it has not documented a general cancellation contract or
  applied the same tolerance to APPEND. It is not a violation of an explicit
  written cancellation guarantee. The honest recommendation is to ask upstream
  whether cancellation is supported generally and, if not, to document it and
  expose a recovery primitive.

### Cancel-safety documentation and recovery API (triage)

Checked async-imap 0.12.0's README, CHANGELOG, crate-level docs and public
method docs (source read only after the first cancellation run):

- **No general cancel-safety statement.** Outside the LOGIN precedent below,
  there are zero occurrences of `cancel`, `cancel-safe`, `abort`, or guidance
  about dropping a command future in the README, CHANGELOG, `src/lib.rs` or
  method docs. Upstream issue search for `cancel` finds only
  [PR #19](https://github.com/chatmail/async-imap/pull/19) (manual interrupt in
  IDLE), not an in-flight command future; `cancel safe`, `abort` and `resync`
  issue searches returned 0. [Issue
  #66](https://github.com/chatmail/async-imap/issues/66) is a closed,
  different corruption path (incomplete FETCH parsing), not cancellation.
- **Partial precedent: LOGIN only, and it is explicitly about a dropped
  future.** The 0.12.0 CHANGELOG says "Do not fail `LOGIN` command when
  receiving a `NO` response for another request" (0.12.0, 2026-09-30).
  [PR #142](https://github.com/chatmail/async-imap/pull/142) (merged
  2026-09-11) explains: "Normally the response should have been read by the
  time login command is sent, so this should not happen. This can still happen
  if the future reading the response was dropped or if the server is violating
  the standard and e.g. duplicates responses." The fix makes
  `login_with_capabilities` loop until the response tag matches its own request
  (`src/client.rs:207-241`); APPEND still reads exactly one response. So
  upstream has recognized and partially fixed this failure class for LOGIN,
  but has not made it a general cancellation contract or fixed APPEND.
- **Recovery surface is close-only.** No public `reset`, `resync`,
  `resynchronize`, `drain`, `abort`, `cancel` or `recover` method exists. The
  public surface is the ordinary IMAP command set plus low-level
  `read_response()` / `run_command()`.
- **`logout(&mut self)`** is documented only as "Logout informs the server that
  the client is done with the connection." It is a graceful close, not a
  resynchronization; it does not consume `self`, and no doc says to call it
  after cancellation or that it makes the handle reusable.
- **`close(&mut self)`** is IMAP CLOSE: it expunges `\Deleted` messages and
  returns to authenticated state. It is not a reset; it sends another command
  through the same parser and does not explicitly drain the abandoned reply
  first.
- **`noop(&mut self)`** is documented as "Noop always succeeds, and it does
  nothing." It sends NOOP and parses until its tagged Done. It is not
  documented as a drain/resync primitive, and the doc's "always succeeds" is
  not a recovery guarantee.
- **`read_response(&mut self)`** is public ("Read the next response on the
  connection") and can drain the abandoned reply — probe B did exactly that —
  but it is a raw primitive, not a documented recovery API, and the caller has
  to know how many responses to consume and what to do with unsolicited data.
- **Drop/close:** there is no `Drop` impl for `Session`; dropping it closes the
  underlying stream (no LOGOUT), and cannot repair the handle. The only
  reliable documented route is to close and reconnect; manually draining raw
  responses worked in probe B but is unsupported. Reconnecting is a new
  session, not survival of this one.

The public documentation therefore neither promises general cancellation
safety nor tells the caller that a cancelled operation outside the LOGIN case
requires draining or reconnecting. That silence is the contract gap: a normal
Rust caller can drop the future (as B3/M1 and any `timeout` wrapper do) and has
no documented signal or supported repair.

Raw grep/signature/search output for this triage:
[logs/cancel-safety-triage.txt](logs/cancel-safety-triage.txt). The
logout/close/drop conclusions above are from docs and signatures; they were not
behavior-tested against a desynchronized connection, so "documented close, not
resync" is the claim, not "logout definitely fails/succeeds."

### Adjacent non-cancellation finding: SEARCH swallows BAD/NO

During the pre calibration, `search` on an unselected session returned
`Ok(0)` while GreenMail logged BAD `SEARCH not valid in this state
AUTHENTICATED` (7 occurrences, matching 7 pre searches). `parse_ids`
(`src/parse.rs:329`) uses `filter`/take_while and never inspects the matching
tagged Done's status, so a server rejection becomes an empty success. This is
not caused by cancellation, but it affects the interpretation of the M1
`pre` observation and is a separate, clear public-API error-handling issue:
`search` should return `Err(Error::Bad)`/`Err(Error::No)`, not `Ok(empty)`.
It is reported here because it was observed in the same run; it is not counted
as a ladder violation and should be triaged separately.

## Coverage

- **Pending boundaries reached**: plans `#0@1:Immediate`, `#0@1:AfterWake`,
  `#0@2:Immediate`, `#0@2:AfterWake`. At `@1` the cancellation happens while
  SELECT waits for the first server chunk (the proxy holds it 50 ms); at `@2`
  the cancelled future has already consumed the first response (consistent with
  the B2 flag pattern) and waits for the next. Both Immediate and AfterWake were
  realized; the race choice did not change the outcome.
- **Realization limits**: `@2` plans were sometimes unrealized because SELECT
  completed before a second Pending (B1 rep 3, B3 rep 1). B3 rep 3 observed only
  one Pending, so only two plans existed. M1 and B2 realized all four plans in
  every repetition. This is real-I/O schedule variance, not a changed
  operation.
- **Same mailbox touched**: every survivor APPEND/SEARCH used `INBOX`; the
  independent observer shows the seed in every run. In the failing cases the
  survivor's message count is 0, so the append did not reach the mailbox. In the
  drained/patched contrast it is 1.
- **Close/reconnect**: no evidence of close or reconnect. The probe used one
  proxy connection and could read the abandoned SELECT responses and the APPEND
  `+` after the failure; the client never reported `ConnectionLost` and never
  re-logged in. The server log shows one connection per session with an
  incomplete APPEND literal, not a reconnect. The B2 `Ok` is achieved by
  re-selecting on the same object, not by a new session.
- **What the next operation observes**: APPEND -> immediate `Error::Append`
  and no effect; SEARCH after that -> timeout because the server is waiting for
  the APPEND literal, and FETCH is not attempted; SELECT retry -> `Ok` with
  merged stale metadata at a first-Pending cut; B1 stop -> no panic/leak, but
  the abandoned server-side state is never observed.
- **Still unexplained / not covered**: whether NOOP, LIST, STATUS, IDLE or a
  second APPEND would consume the stale response differently; whether a retry
  SELECT followed by APPEND truly recovers; whether other servers (Dovecot,
  Cyrus) chunk SELECT responses differently and shift the failure; cancellation
  at every later Pending up to completion; TLS, pipelining, multiple in-flight
  commands, other async-imap versions, and upstream tests. The patch is a
  triage sketch, not a validated fix.

## Productization friction (Dropwise)

- `Settle::ZERO` is not in the public API; the task's shorthand has to be
  translated to a struct literal. Small docs/API mismatch.
- B2/B3 need to record a concrete per-trial value while still using plain
  `explore` (which only carries `Ok/Err`). This runner uses an
  `Arc<Mutex<Vec<String>>>` side channel and assumes the execution order
  matches `Report.trials`; it worked because every run was correlated, but it
  is brittle research boilerplate. A per-trial note/actual channel (or using
  `explore_observed` with a non-oracle observation) would be a real
  productization improvement.
- The shared ladder reporter is outside the crate and imported with
  `#[path = "../../../../support/ladder.rs"]`; the relative depth is easy to
  get wrong (four levels up from `src`/`tests`, not three). The exact path was
  verified with `realpath` and compiled. A small published helper or a
  documented canonical path would remove this friction.
- `Report`'s `plans=` field is the number of trials, not distinct cut points;
  this is understandable from the diagnostics but easy to misread in a summary.
- `Trial::replay_plan` and `replay`/`replay_observed` exist and were not used
  here. They are the right next tool for the B3/M1 plans; the current logs give
  the exact plans to replay.
- Dropwise's diagnostics were genuinely useful: `report.diagnostics()` printed
  the full Obs pre/post comparison, which is what made the "Neither" verdict
  legible. No Dropwise API bug was found in this run.
- Documentation opportunity for the main agent: `Ctx::target` drops a future,
  but a Pending cut can be after the request has left the client and the server
  has completed it. Only the explicit `pre` calibration is "never sent". A
  sentence about this in the scenario-author docs would prevent
  "cancelled == not started" assumptions. This client's M1 pre/post/neither
  split is a concrete example.

## Cost

- Clean runner wall time: **89 s** total (preflight 3 s, probe 1 s, rounds
  28 + 29 + 28 s). Build: 8.17 s. One container, no reruns needed for the clean
  data. The earlier exploratory runs against a throwaway container took a few
  minutes and are not part of the reported logs.
- Ladder volume: 46 planned trials, 12 baselines, 12 M1 calibration samples,
  plus preflight/probe; 22 failed APPENDs (server-confirmed), mailbox grew to
  96 messages by the end of round 3.
- Active investigation including setup, harness, preflight, triage probe and
  patch contrast: well under the 2.5 h box.

## Limitations

- One client version (0.12.0), one server (GreenMail 2.1.14), one mailbox, one
  operation shape (direct handle, single in-flight command). Not a proof about
  async-imap, IMAP clients in general, or other servers.
- The Pending count depends on real I/O and proxy chunking; `@2` plans can be
  unrealized. Only the first two observed Pending boundaries were exercised;
  later boundaries and cancellation just before completion were not.
- The probe uses `tokio::time::timeout` to drop SELECT rather than
  `Ctx::target`; the drop is the same mechanism, but the probe is one sample
  and is triage, not the ladder.
- The dependency-copy patch is one hand-written change, not an upstream PR or
  a tested fix. It shows the consequence is attributable to the APPEND
  continuation read, not that the patch is safe in general.
- The M1 pre observation's `search = Ok(0)` is a swallowed BAD (adjacent
  finding), so pre is not a clean "zero matches" state; the observer, append and
  fetch fields still distinguish pre from post.
- The recovery-API triage is documentation/API-surface only. `logout`, `close`
  and `noop` after this specific failure were not behavior-tested; the claim is
  that none is documented or shaped as an in-place resync, not that a particular
  call would definitely fail.
- Absolute message counts/UIDs/seqs grow across runs and are normalized out of
  the observation; the REST observer reads all messages and filters by the
  current run id. No cleanup between runs.
- Upstream issue search was limited (GitHub search rate limit after a few
  queries); absence of a matching issue is not proof that none exists.

## Next concrete experiment

1. Replay the exact B3/M1 plans with `Trial::replay_plan` and
   `dropwise::replay`/`replay_observed` to confirm the same result without a
   new search, and keep the replay logs.
2. Add a three-repetition caller-side drain phase (drain the abandoned reply
   before APPEND) and record `Proxy::max_residue()` before and after, so the
   residue is quantified, not just observed as queued responses.
3. Test retry SELECT followed by APPEND on the same session (B2 then B3) to see
   whether the `Ok` retry truly restores the session and whether duplicated
   flags remain the only contamination.
4. Test a first survivor that is NOOP / LIST / STATUS / SEARCH, one per plan, to
   map which response parsers skip stale tagged/untagged data
   (`parse_ids`, `parse_fetches`, `parse_names`, `parse_noop` all share the
   `filter`/take_while pattern), and whether any of them also swallow BAD/NO.
5. Run the same ladder against Dovecot (or another local IMAP server) to test
   response chunking sensitivity, and cancel at every Pending boundary up to
   completion (longer reply delay / more chunks).
6. Report the minimal APPEND continuation patch and the duplicated-`Mailbox`
   observation to async-imap upstream as a question/bug; do not open an issue
   from this exploration without the main agent's decision.

## Files to review

- [SCENARIO.md](SCENARIO.md) — preregistration plus post-run addendum.
- [src/main.rs](src/main.rs) — harness, probe, levels; `run.sh` is the entry.
- [run.sh](run.sh) — one disposable container, three rounds, raw logs.
- [environment.txt](environment.txt) — versions and image digest.
- [logs/preflight.stdout.log](logs/preflight.stdout.log) — positive controls.
- [logs/probe.stdout.log](logs/probe.stdout.log) — queued-response mechanism.
- [logs/round-1.stdout.log](logs/round-1.stdout.log),
  [round-2](logs/round-2.stdout.log), [round-3](logs/round-3.stdout.log) —
  full ladder diagnostics.
- [logs/greenmail.log](logs/greenmail.log) — server-side 22 APPEND warnings.
- [logs/patch-contrast.txt](logs/patch-contrast.txt) — triage patch contrast.
- [logs/cancel-safety-triage.txt](logs/cancel-safety-triage.txt) — cancel-safety
  docs grep, method signatures, PR #142 and upstream search evidence.
---

# Round 2 (2026-10-21): recovery chain, patch validation, rerun

Status: **round-2 expansion, appended after the round-1 fragment**. The round-1
text above is unchanged. Round-2 runs used separate processes, separate
containers and **round2-*** log names; their plan/violation counts are not added
to the round-1 denominators.

## Questions and what was added

1. Does a retry SELECT after a realised cancelled SELECT actually restore the
   same Session for APPEND -> SEARCH/FETCH? Three repetitions of the
   recovery-first order (retry SELECT, then APPEND) and three of the reverse
   order (APPEND, then retry SELECT), with a 50 ms proxy, real-time
   MultiThread(1), and a fresh GreenMail container per dependency variant.
2. Validate the round-1 APPEND-continuation sketch: normal APPEND, server
   NO/BAD, EOF/connection close, stale tagged/untagged SELECT responses, stale
   continuation. Add regression tests that fail on published 0.12.0 and pass on
   the patch.
3. Re-run B1/B2/B3/M1 three times each against published and patched async-imap,
   keeping round-2 counts separate. Does the patch remove B3/M1 APPEND failures,
   and does B2 flag duplication remain?
4. Minimal SEARCH-on-unselected reproduction, separate from APPEND.

New harness files: [round2/src/main.rs](round2/src/main.rs) (phases
preflight, chain, chain-reverse, ladder, patch-behavior, search-unselected;
raw-byte TeeStream transcript; 2 s bound for the reverse retry/search),
[run-round2.sh](run-round2.sh), and the dependency-copy files under
[patched/](patched/). Commands and environment:
[logs/round2-commands.txt](logs/round2-commands.txt),
[logs/round2-environment.txt](logs/round2-environment.txt). The container image
was greenmail/standalone:2.1.14, digest
sha256:1ef95a966418cd09b7ea91d504d8c0826bbe7a2f6e679a75c601a831587c1626.
Published and patched builds both report async-imap 0.12.0; the patched build
resolves it from the copied dependency (cargo tree shown in
[logs/round2-build-patched.log](logs/round2-build-patched.log)).

## 1. Recovery chain

Each row is three repetitions; "plans" is generated cancellations and
"realised" is the subset where the target was actually dropped at a Pending.
"Observer 2" means the independent REST view saw the seed and the uniquely
tagged survivor. "Timeout" in the reverse order is the 2 s bound on the retry
SELECT, not a 5 s operation timeout.

| variant | order | rep | plans | realised | unrealised | retry SELECT | APPEND | SEARCH/FETCH | observer |
|---|---|---|---:|---:|---:|---|---|---|---|
| published | retry first | 1 | 4 | 4 | 0 | Ok | Ok | Ok(1) / Ok | 2 |
| published | retry first | 2 | 2 | 2 | 0 | Ok | Ok | Ok(1) / Ok | 2 |
| published | retry first | 3 | 4 | 4 | 0 | Ok | Ok | Ok(1) / Ok | 2 |
| published | APPEND first | 1 | 4 | 3 | 1 | Err(Timeout) | Err(Append) | Timeout / not attempted | 1 |
| published | APPEND first | 2 | 4 | 4 | 0 | Err(Timeout) | Err(Append) | Timeout / not attempted | 1 |
| published | APPEND first | 3 | 4 | 4 | 0 | Err(Timeout) | Err(Append) | Timeout / not attempted | 1 |
| patched | retry first | 1 | 4 | 4 | 0 | Ok | Ok | Ok(1) / Ok | 2 |
| patched | retry first | 2 | 4 | 4 | 0 | Ok | Ok | Ok(1) / Ok | 2 |
| patched | retry first | 3 | 4 | 4 | 0 | Ok | Ok | Ok(1) / Ok | 2 |
| patched | APPEND first | 1 | 4 | 4 | 0 | Ok | Ok | Ok(1) / Ok | 2 |
| patched | APPEND first | 2 | 4 | 3 | 1 | Ok | Ok | Ok(1) / Ok | 2 |
| patched | APPEND first | 3 | 4 | 4 | 0 | Ok | Ok | Ok(1) / Ok | 2 |

All 44 realised chain runs had one proxy connection before and after
("conns=1"), and the same Session/stream addresses. No run reconnected. The
single unrealised plan in each reverse set is a baseline where SELECT completed
before the planned Pending; it is excluded from the effect claims.

### Does retry SELECT restore the session?

**Yes for command alignment in the retry-first order, but not cleanly.** Every
realised published retry SELECT returned Ok, and the following APPEND, SEARCH,
FETCH and REST observer all succeeded on the same Session and same TCP
connection. The returned Mailbox, however, is contaminated: at first-Pending
cuts flags_len=10 and permanent_flags_len=12; at second-Pending cuts
flags_len=5 and permanent_flags_len=12. The normal values are 5 and 6.

**No in the published APPEND-first order.** APPEND returns Err(Append) at once
and leaves the server waiting for the APPEND literal. The subsequent retry
SELECT bytes are consumed as literal data; the retry times out at 2000-2001 ms,
SEARCH times out at 2000-2002 ms, FETCH is not attempted, and the observer sees
only the seed. Order matters: a retry SELECT before APPEND can resynchronise far
enough to make the next command work; a retry SELECT after the failed APPEND
cannot.

**The patch changes the reverse order, not the retry-first metadata.** With the
patched APPEND, the reverse order drains the abandoned SELECT, succeeds, and the
following retry SELECT returns Ok with clean 5/6 flags; all 11 realised reverse
runs completed APPEND/SEARCH/FETCH and the observer saw 2. In the retry-first
order, the retry SELECT still runs before the patched APPEND and still uses
parse_mailbox, so flags remain 10/12 at first-Pending cuts and 5/12 at
second-Pending cuts.

### Duplicated flags: accumulation in the client, not the server

The raw retry transcript shows SELECT A0004 reading the abandoned SELECT's
FLAGS/PERMANENTFLAGS and stale A0003 OK, then its own FLAGS/PERMANENTFLAGS and
A0004 OK. The server sends one FLAGS list per SELECT; it does not send duplicate
flags. parse_mailbox starts from Mailbox::default() and uses extend() for FLAGS
and PERMANENTFLAGS, so the abandoned list and the new list accumulate.
Mailbox.flags and Mailbox.permanent_flags are public Vec<Flag> fields, so this is
user-visible as length/content (10/12, with repeated entries), although
membership tests are unchanged. It is a client-side metadata defect triggered by
the abandoned SELECT, not an independent server defect, and it is not counted as
a B2 violation because B2 judged only Ok.

Representative first-Pending value (published and patched retry-first):

    retry = Ok(Mailbox {
      flags = [Answered, Deleted, Draft, Flagged, Seen,
               Answered, Deleted, Draft, Flagged, Seen],     // 10
      permanent_flags = [Answered, Deleted, Draft, Flagged, Seen, MayCreate,
                         Answered, Deleted, Draft, Flagged, Seen, MayCreate] // 12
    })

At a second-Pending cut flags are 5 and permanent_flags are 12; the stale
PERMANENTFLAGS response is still merged even when FLAGS was already consumed.
This refines the round-1 observation (which recorded 6/6 first cuts with ten
flags and 6/6 second cuts with five) by naming the mechanism and showing the
permanent-flags duplication in both cases.

## 2. APPEND-continuation patch

The patch is the round-1 loop, unchanged in behavior, with comments and a
regression suite. It is at
[patched/append-continuation.round2.patch](patched/append-continuation.round2.patch)
(30 lines; tests at
[patched/append-continuation-tests.round2.rs](patched/append-continuation-tests.round2.rs)).
It replaces the single "next response must be Continue" read with a loop:
Continue -> break; Done for this command's tag -> Err(Append); Done for another
tag or any other response -> continue; EOF -> Err(Append). This is the same
tag-first discipline that async-imap PR #142 added to LOGIN
(https://github.com/chatmail/async-imap/pull/142, diff saved as
[logs/round2-pr142.diff](logs/round2-pr142.diff)): PR #142 matches
Response::Done { tag } == id before calling check_status_ok, so another
command's NO does not fail LOGIN. APPEND differs because its own tagged Done
before Continue cannot be a success: no literal was sent, so it is an error.

### Real-server behavior

| case | published | patched |
|---|---|---|
| normal selected | Ok, 145 ms, observer 1 | Ok, 237 ms, observer 1 |
| normal unselected | Ok, 104 ms, observer 1 | Ok, 105 ms, observer 1 |
| nonexistent mailbox (NO after +) | Err(No), 146 ms, observer 0 | Err(No), 107 ms, observer 0 |
| invalid flags (BAD before +) | Err(Append), 52 ms, observer 0 | Err(Append), 52 ms, observer 0 |
| invalid date (BAD before +) | Err(Append), 51 ms, observer 0 | Err(Append), 52 ms, observer 0 |
| stale SELECT then APPEND | Err(Append), 0 ms, observer 0 | Ok, 237 ms, observer 1 |
| stale SELECT then rejected APPEND | Err(Append), 0 ms, observer 0 | Err(No), 104 ms, observer 0 |
| fake EOF before + | Err(Append), 40 ms | Err(Append), 41 ms |
| fake own-tag NO before + | Err(Append), 40 ms | Err(Append), 40 ms |
| fake stale + | caller timeout, 2001 ms | caller timeout, 2001 ms |

The patched stale-SELECT transcript shows eight stale SELECT responses plus the
stale tagged A0002 OK skipped, then + OK, the literal, and A0003 OK
[APPENDUID ...]. The patched stale-reject transcript shows the same drain, then
A0003 NO [TRYCREATE]. Thus the patch removes the stale-response false failure,
preserves normal APPEND, returns promptly on NO/BAD/EOF/own-tag completion, and
does not swallow the real tagged completion.

### Regression and upstream checks

- Published 0.12.0 + injected tests: **10 passed, 3 failed**. The failures are
  round2_append_skips_stale_select_responses,
  round2_append_skips_stale_no_for_other_command and
  round2_append_skips_stale_bad_for_other_command; all fail with Err(Append).
  Upstream tests with round-2 tests skipped: **73 passed, 0 failed**.
- Patched copy: **86 passed, 0 failed** (73 upstream + 13 round-2).
- cargo fmt --all -- --check: **0** on both patched and published copies.
- cargo clippy --all-targets: **0** on both; the two warning sets are identical
  (5 unique warnings each, empty diff), so the patch adds no lint warnings.

Raw logs:
[logs/round2-patch-tests-summary.txt](logs/round2-patch-tests-summary.txt),
[logs/round2-patch-tests-unpatched.log](logs/round2-patch-tests-unpatched.log),
[logs/round2-patch-tests-patched.log](logs/round2-patch-tests-patched.log).
Patch limitations are in section 5.

## 3. Ladder rerun (round-2 denominators)

plans = Report.trials.len(); the baseline is not a trial. Every row had
baseline_errors=0, unsettled=0, exhaustive=true and baseline_targets=1.

| variant | level | rep | plans | violations | unrealised | actual |
|---|---|---|---:|---:|---:|---|
| published | B1 | 1 | 4 | 0 | 2 | baseline cuts only |
| published | B1 | 2 | 4 | 0 | 2 | baseline cuts only |
| published | B1 | 3 | 4 | 0 | 0 | no panic/timeout |
| published | B2 | 1 | 2 | 0 | 0 | 2/2 retry Ok; both first-Pending: flags 10, perm 12 |
| published | B2 | 2 | 4 | 0 | 0 | 4/4 Ok; first cuts flags 10, second cuts flags 5, perm 12 |
| published | B2 | 3 | 4 | 0 | 1 | 3/3 Ok; first cuts flags 10, second cut flags 5, perm 12 |
| published | B3 | 1 | 4 | 4 | 0 | 4/4 Err(Append), observer 0 |
| published | B3 | 2 | 4 | 3 | 1 | 3/3 realised Err(Append), observer 0 |
| published | B3 | 3 | 4 | 3 | 1 | 3/3 realised Err(Append), observer 0 |
| published | M1 | 1 | 4 | 4 | 0 | 4/4 Neither |
| published | M1 | 2 | 4 | 4 | 0 | 4/4 Neither |
| published | M1 | 3 | 4 | 4 | 0 | 4/4 Neither |
| patched | B1 | 1 | 4 | 0 | 1 | baseline cuts only |
| patched | B1 | 2 | 4 | 0 | 0 | no panic/timeout |
| patched | B1 | 3 | 4 | 0 | 2 | baseline cuts only |
| patched | B2 | 1 | 4 | 0 | 0 | 4/4 Ok; first cuts flags 10, second cuts flags 5, perm 12 |
| patched | B2 | 2 | 4 | 0 | 2 | 2/2 realised first-Pending: flags 10, perm 12 |
| patched | B2 | 3 | 4 | 0 | 2 | 2/2 realised first-Pending: flags 10, perm 12 |
| patched | B3 | 1 | 4 | 0 | 0 | 4/4 Ok, observer 1 |
| patched | B3 | 2 | 4 | 0 | 0 | 4/4 Ok, observer 1 |
| patched | B3 | 3 | 2 | 0 | 0 | only one Pending observed; 2/2 Ok, observer 1 |
| patched | M1 | 1 | 4 | 0 | 2 | post=4; 2 realised + 2 baseline, all Post |
| patched | M1 | 2 | 4 | 0 | 2 | post=4; 2 realised + 2 baseline, all Post |
| patched | M1 | 3 | 4 | 0 | 0 | post=4; 4/4 realised, all Post |

Totals: published round 2 = **46 plans, 22 violations, 7 unrealised**.
Patched round 2 = **46 plans, 0 violations, 11 unrealised**. Published round 1
was 46/21/2; the realisation schedule differed, so the rounds are not pooled.

### B2 after the patch

Every realised patched B2 first-Pending cut still returned 10 flags and 12
permanent flags; every realised second-Pending cut returned 5 flags and 12
permanent flags. The APPEND patch does not touch SELECT/parse_mailbox, so the
duplication persists exactly as in the published build. This is the answer to
"does B2 flag duplication remain": yes.

### B3 and M1 after the patch

Published B3 had 10 realised cancellations (4+3+3), all Err(Append) and observer
0; patched B3 had 10 realised cancellations (4+4+2), all Ok and observer 1. The
patch removes all 10 realised B3 APPEND failures. The patched B3 third
repetition generated only two plans because only one Pending was observed; both
succeeded.

Published M1 was 12/12 realised and 12/12 Neither. Representative:

    Obs { append: Err("Append"), search: Err("Timeout"),
          fetch_text: Err("Timeout"),
          observer_subjects: Ok(["s-RUN-seed"]),
          observer_seed_body: true, observer_survivor_body: false,
          observer_run_message_count: 1 }

Patched M1 had 12 plan observations, 8 realised and 4 baseline, all 12 Post
(append Ok, search Ok(1), fetch Ok, observer seed+survivor); violations=0,
neither=0. The four baseline rows are not counted as recovered cancellations.

## 4. SEARCH on an unselected mailbox (separate)

Both builds reproduce the same independent error-handling defect:

    before SELECT: search = Ok(0), raw="seqs=[]", 92 ms
    server log:   Command 'SEARCH' not valid in this state AUTHENTICATED
    after SELECT:  search = Ok(0), raw="seqs=[]", 51 ms (genuine empty result)

GreenMail rejects the first SEARCH with BAD; parse_ids ends at the matching
tagged response without checking its status, so the client returns Ok(empty).
This is not cancellation-related and is not counted in the ladder. Logs:
[logs/round2-unpatched-search-unselected.stdout.log](logs/round2-unpatched-search-unselected.stdout.log)
and its server delta
[logs/round2-unpatched-search-unselected-server.log](logs/round2-unpatched-search-unselected-server.log).

## 5. Limitations and unproven items

- **Stale continuation is unsolvable by this patch.** IMAP "+" is untagged and
  carries no command tag. If an abandoned APPEND's "+" is still in the queue,
  the patch cannot tell it from this APPEND's "+"; both builds then wait until
  the caller's timeout (2 s in patch-behavior). A cancelled APPEND continuation
  still needs a reconnect or a protocol-level recovery primitive. This patch is
  not that primitive.
- **Retry SELECT metadata is not fixed.** The duplication is client-side
  accumulation; the patch is APPEND-only. A caller that reuses the returned
  Mailbox sees 10/12 or 5/12 flags.
- **NO/BAD before + loses its status detail.** The patch returns Err(Append)
  promptly (51-52 ms in the real-server cases) instead of Err(No)/Err(Bad). It
  still errors rather than succeeding or hanging, but a caller cannot tell a
  rejected APPEND header from a protocol/continuation failure.
- **A stale tagged NO/BAD after + is not covered.** The patch stops at the
  first +; the literal is then written and the completion is read by
  check_done_ok_from, which checks status before tag. An unrelated tagged
  NO/BAD arriving between + and this APPEND's own completion can therefore be
  reported as this APPEND's failure. This ordering did not occur on GreenMail in
  the round-2 runs; the limitation is inferred from the 0.12.0 source and is not
  behavior-tested.
- **Only GreenMail 2.1.14** was used, with a 50 ms proxy. Response chunking is
  not controlled; the first/second-Pending split is the observed plan set, not
  a protocol guarantee. Dovecot/Cyrus, TLS, pipelining, multiple in-flight
  commands, other async-imap versions and longer cancellation ladders were not
  tested.
- **Plan counts are schedule-dependent.** Repetitions produced 2 or 4 B2/B3
  plans and unrealised baselines. The table reports actual counts; no claim is
  made that four plans are guaranteed. Unrealised plans are excluded from effect
  claims.
- **The patch is a dependency-copy triage change**, not an upstream fix and not
  reviewed by async-imap maintainers. It changes no file in the repository's
  Cargo dependency graph; the runner copies the crate to /tmp and applies
  [patched/apply-round2-patch.sh](patched/apply-round2-patch.sh).
- **User-visible impact of duplicated flags is not tested in a client.** The
  fields are public, so duplicates are observable, but no consumer behavior
  beyond membership was tested.
- **Server log accounting is from GreenMail.** Published: 35 incomplete APPEND
  literals (10 B3 + 12 M1 + 11 reverse chain + 2 stale-SELECT patch cases), 7
  SEARCH BAD lines (1 search-unselected + 6 M1 pre calibrations), 3 broken
  pipes. Patched: 0 incomplete APPEND literals, 7 SEARCH BAD lines, 3 broken
  pipes. Logs:
  [logs/round2-greenmail-unpatched.log](logs/round2-greenmail-unpatched.log),
  [logs/round2-greenmail-patched.log](logs/round2-greenmail-patched.log).

## 6. Round-2 artifacts

- Harness: [round2/](round2/), runner [run-round2.sh](run-round2.sh).
- Exact commands: [logs/round2-commands.txt](logs/round2-commands.txt).
- Environment: [logs/round2-environment.txt](logs/round2-environment.txt).
- Per-phase stdout/stderr and GreenMail logs:
  [logs/round2-*](logs/).
- Summary table: [logs/round2-summary.txt](logs/round2-summary.txt).
- Patch/test files: [patched/](patched/); test evidence:
  [logs/round2-patch-tests-summary.txt](logs/round2-patch-tests-summary.txt).
- PR #142 diff: [logs/round2-pr142.diff](logs/round2-pr142.diff).
- Raw chain transcripts are the CHAIN-TX lines in
  [logs/round2-unpatched-chain-1.stdout.log](logs/round2-unpatched-chain-1.stdout.log)
  and [logs/round2-patched-chain-reverse-1.stdout.log](logs/round2-patched-chain-reverse-1.stdout.log).


## 收尾复核

2026-10-21 的独立审查、runner 修正和补跑结果见 [总报告 §9](../../RESULTS.md#9-收尾复核2026-10-21)。
本片段此前的数字是第二轮历史结果，不替代最新的补丁边界和验证结论。
