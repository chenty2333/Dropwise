# RESULTS FRAGMENT — async-smtp 0.10.2 direct handle, GreenMail

Phase 4 exploration, round 1. Completed 2026-10-21; official ladder
03:06:24–03:13:04 Asia/Tokyo. Work confined to
prospective/phase4-exploration/clients/smtp/. No commit, push, publish, issue
or PR. No Dropwise core file modified.

> **Round-2 addendum (below).** The round-1 measurements are unchanged. The
> addendum corrects three round-1 interpretations: M1 did not carry send codes,
> so it could not detect the 354/effect mismatch; the third send does not consume
> a stale 250; and a dependency-copy fix plus old-vs-new-transport contrast now
> exist. It also distinguishes plan violations, user-visible phenomena and root
> causes.

## Bottom line

1. B1 (cancel then stop) is clean: 8 plans x 3 reps, 0 violations, 0 baseline
   errors, no panic, timeout or obligation leak.
2. Reuse is not safe. In all six B2/B3 repetitions, 6 of 8 cancellation plans
   return Ok(code=354, message=["Start mail input; end with <CRLF>.<CRLF>"]),
   the intermediate DATA prompt, and the 2 plans at #0@3 never return; Dropwise
   kills them at the 20 s liveness timeout. M1 shows the same 2 timeouts.
3. The 354 does not mean the survivor's mail was lost. M1's independent REST
   observer sees the survivor message in every plan where it runs:
   pre=4 post=2 mixed=0 neither=0 unknown=0 in each of 3 reps. At #1/#2 only the
   survivor message is present (Pre); at #4 both target and survivor messages
   are present (Post), even though the target was cancelled.
4. The 354 is a false-success shape: the server accepted the message, but send
   returned before the final 250 was consumed, leaving it queued. A caller that
   checks only is_ok(), as B2/B3 required, cannot see this. A stricter
   Ok-and-code-250 oracle would flag 6 more plans per B2/B3 rep.
5. Source read after the first violation-shaped result explains the shift:
   read_response accepts any positive reply (2xx/3xx), and message() returns
   that reply as the final send result. A shifted read can therefore hand the
   354 to send. No public cancellation-safety, poisoning or reset contract
   exists.

## Versions and environment

From environment.txt and the logs:

- rustc 1.98.0 (88d9e12ae 2026-08-18); cargo 1.98.0 (797e8a9bc 2026-08-05).
- Docker 29.8.2.
- GreenMail image greenmail/standalone:2.1.14; image id
  sha256:1ef95a966418cd09b7ea91d504d8c0826bbe7a2f6e679a75c601a831587c1626;
  digest greenmail/standalone@sha256:1ef95a966418cd09b7ea91d504d8c0826bbe7a2f6e679a75c601a831587c1626.
  GreenMail standalone v2.1.14 in its log.
- async-smtp v0.10.2, default features (runtime-tokio, tokio), exact pin.
- Dropwise path ../../../.. (repository root), feature net, commit
  e841a60b825e0e66ecf50033fe906875c28e84f8.
- Locked: tokio 1.54.0, ureq 2.12.1, serde_json 1.0.154.
- Container dw-p4-smtp-726148; SMTP 127.0.0.1:32787; REST
  http://127.0.0.1:32789; loopback only; removed by trap.
- GreenMail options: -Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0
  -Dgreenmail.auth.disabled
  -Dgreenmail.users=smtpuser:pass@example.com,dwother:pass@example.com.
  Each scenario run creates its own two REST users, so mailboxes are isolated.

## Commands and artifacts

    cd prospective/phase4-exploration/clients/smtp
    bash run.sh

run.sh starts the disposable container, waits for /api/user, writes
environment.txt, runs prep, then 12 level x rep runs, writes logs/LADDER.txt,
and removes the container in an EXIT trap. The runner is a harness=false test
target (tests/levels.rs) using prospective/support/ladder.rs read-only via
#[path = "../../../../support/ladder.rs"] (four levels up, as corrected).

Artifacts: SCENARIO.md, tests/levels.rs, run.sh, environment.txt,
logs/LADDER.txt, logs/SUMMARY.md, logs/ (raw stdout/stderr/status), and
summarize.py.

## Preflight evidence

### No cancellation: two sends + observer

    PREP-NOCANCEL first=Ok(code=250, message=["OK"])
    PREP-NOCANCEL second=Ok(code=250, message=["OK"])
    PREP-NOCANCEL transport_ptr=0x7ffc0cb41138 same_object=true
      connections=1 ["client=127.0.0.1:54240 up=920 down=255"]
    PREP-NOCANCEL observation=Observation { count: 2,
      messages: [subject "p4-smtp <run> nocancel-one",
                 message_id "<<run>.nocancel-one@dw.test",
                 body "body <run> nocancel-one"],
                [subject "p4-smtp <run> nocancel-two",
                 message_id "<<run>.nocancel-two@dw.test",
                 body "body <run> nocancel-two"] }

Both 250, one connection, exactly two expected messages.

### Reuse / identity diagnostic

    PREP-REUSE first=Ok(code=250, message=["OK"])
    PREP-REUSE second=Ok(code=250, message=["OK"])
    PREP-REUSE transport_ptr=0x7ffebb226440 same_object=true
    PREP-REUSE before=connections=1 ["client=127.0.0.1:33162 up=18 down=115"]
    PREP-REUSE after_first=connections=1 ["client=127.0.0.1:33162 up=439 down=185"]
    PREP-REUSE after_second=connections=1 ["client=127.0.0.1:33162 up=860 down=255"]
    PREP-REUSE same_tcp_connection=true one_connection=true bytes_grew=true
    PREP-REUSE observation=Observation { count: 2, ... }

Same client address and exactly one proxy connection before, between and after
the two sends; same transport object; no close, reconnect, login, NOOP or reset.

### Pre/post calibration

    PREP-CALIBRATION pre_survivor=Ok(code=250, message=["OK"])
    PREP-CALIBRATION pre=Observation { count: 1, ... survivor only ... }
    PREP-CALIBRATION post_target=Ok(code=250, message=["OK"])
    PREP-CALIBRATION post_survivor=Ok(code=250, message=["OK"])
    PREP-CALIBRATION post=Observation { count: 2, ... target, survivor ... }
    PREP-CALIBRATION distinguishable=true pre_count=1 post_count=2

The observer is sensitive to whether the target ran before cancellation.

## Ladder results (official, 3 reps each)

Shared Config: Flavor::MultiThread { workers: 1 }; races [Immediate,
AfterWake]; max_cancellations=1; max_runs=64; scenario_timeout=20 s; settle
zero tokio and real time (Settle::ZERO is not public; set the two Durations to
ZERO explicitly); proxy reply_delay=50 ms.

| level | rep | plans | violations | unrealized | unsettled | baseline_errors | actual B2/B3 survivor result | M1 verdicts |
|---|---|---|---|---|---|---|---|---|
| B1 | 1 | 8 | 0 | 0 | 0 | 0 | — | — |
| B1 | 2 | 8 | 0 | 0 | 0 | 0 | — | — |
| B1 | 3 | 8 | 0 | 0 | 0 | 0 | — | — |
| B2 | 1 | 8 | 2 | 0 | 0 | 0 | baseline 250; 6x 354; 2x timeout | — |
| B2 | 2 | 8 | 2 | 0 | 0 | 0 | baseline 250; 6x 354; 2x timeout | — |
| B2 | 3 | 8 | 2 | 0 | 0 | 0 | baseline 250; 6x 354; 2x timeout | — |
| B3 | 1 | 8 | 2 | 0 | 0 | 0 | baseline 250; 6x 354; 2x timeout | — |
| B3 | 2 | 8 | 2 | 0 | 0 | 0 | baseline 250; 6x 354; 2x timeout | — |
| B3 | 3 | 8 | 2 | 0 | 0 | 0 | baseline 250; 6x 354; 2x timeout | — |
| M1 | 1 | 8 | 2 | 0 | 0 | 0 | — | pre=4 post=2 mixed=0 neither=0 unknown=0 |
| M1 | 2 | 8 | 2 | 0 | 0 | 0 | — | pre=4 post=2 mixed=0 neither=0 unknown=0 |
| M1 | 3 | 8 | 2 | 0 | 0 | 0 | — | pre=4 post=2 mixed=0 neither=0 unknown=0 |

All rows exhaustive=true, baseline_targets=1, unrealized=0, unsettled=0,
baseline_errors=0. B1's 9 runs per rep comprise baseline 250 plus 8 cancelled
targets; no B1-RUN error, no panic/leak.

Exact completed-send values:

Baseline, every B2/B3 rep:

    B2-RUN target=Ok(code=250, message=["OK"]) survivor=Ok(code=250, message=["OK"]) connections=1
    B3-RUN target=Ok(code=250, message=["OK"]) survivor=Ok(code=250, message=["OK"]) recipient=<run>-b@example.com connections=1

All six non-#0@3 plans, every B2/B3 rep:

    survivor=Ok(code=354, message=["Start mail input; end with <CRLF>.<CRLF>"]) connections=1

The two #0@3 plans, every B2/B3 rep:

    LADDER-VIOLATION B2 repN [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
    LADDER-VIOLATION B2 repN [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)

B3 is identical in shape, with a different recipient (<run>-b@example.com) and
different subject/body/Message-ID. No other error string occurred. Every
completed run had connections=1; no reconnect or new client address.

Plan-level mapping, deterministic across 3 reps:

| plan | race | B2/B3 survivor | M1 observer |
|---|---|---|---|
| baseline | — | 250 | count 2 (target + survivor) |
| #0@1 | Immediate | 354 | Pre: count 1, survivor only |
| #0@1 | AfterWake | 354 | Pre: count 1, survivor only |
| #0@2 | Immediate | 354 | Pre: count 1, survivor only |
| #0@2 | AfterWake | 354 | Pre: count 1, survivor only |
| #0@3 | Immediate | timeout 20 s | no observation; scenario killed |
| #0@3 | AfterWake | timeout 20 s | no observation; scenario killed |
| #0@4 | Immediate | 354 | Post: count 2, target + survivor |
| #0@4 | AfterWake | 354 | Post: count 2, target + survivor |

M1 does not record the survivor SmtpResult per plan; the 354 in that column is
the same deterministic plan's B2/B3 value, not read from M1. The M1 observer
column is exact. Normalised M1 observations (all reps identical):

Pre: Observation { count: 1, messages: [MessageView { subject:
"p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test",
body: "body <run> survivor" }] }

Post: Observation { count: 2, messages: [target, survivor] }, target subject
"p4-smtp <run> target", id "<<run>.target@dw.test", body "body <run> target";
survivor values as above.

## The 2/8 #0@3 liveness detail

Both races at pending boundary 3 (#0@3) never finish. They are not errors and
not panics: the scenario future is still waiting when Dropwise's 20 s
scenario_timeout fires. The LADDER line is verdict=None with
detail=scenario did not finish within 20s (liveness). This repeats in all B2,
B3 and M1 reps (18 timeouts total). B1 has no timeout at #0@3 because it never
reads or writes again after cancellation; it drops the transport.

B1 proxy byte trace for the four boundaries (representative rep 1; setup is 18
bytes up, 115 down):

| plan | up | down | interpretation |
|---|---|---|---|
| #0@1 | 18 | 115 | target wrote nothing through proxy |
| #0@2 | 57 | 123 | MAIL command (39 bytes) reached proxy |
| #0@3 | 94 | 131 | MAIL + RCPT (37 more) reached proxy |
| #0@4 | 100 | 177 | MAIL + RCPT + DATA (6 more); body not through proxy |

These counters are indicative, not wire-level. Exact SMTP command mapping was
not recorded.

## What the 354 and the extra 250 mean

Public API shape (docs.rs 0.10.2 and examples/send.rs):

- SmtpTransport::send(&mut self, email) -> SmtpResult, where
  SmtpResult = Result<Response, Error>. It borrows the handle; this is a real
  direct handle. Docs say only "Sends an email".
- Public SmtpTransport methods: new, get_mut, get_ref, into_inner, try_login,
  starttls, quit, auth, send. There is no reset/rset/reconnect method. quit is
  async and sends QUIT; into_inner returns the stream. The canonical example
  uses a plain BufStream<TcpStream> with no read/write timeout.
- README, public docs and the published source tree contain no
  cancellation-safety or poisoned-handle statement; there is no CHANGELOG.
  Upstream issue #57 (closed) leaves buffering, read/write timeouts and TLS to
  the caller. No issue matching cancellation/354 was found in 72 issues
  inspected.

Source mechanism (registry copy, read-only, after the first violation):

- src/stream.rs:70-99: read_response returns Ok for any positive reply, so any
  2xx or 3xx, including 354.
- src/stream.rs:102-120: message() writes body plus terminator, then returns
  read_response() directly. It does not require the final reply to be 250 or
  even 2xx.
- src/smtp_client.rs:213-285: send sends MAIL/RCPT/DATA then calls message().
  Non-pipelined branch reads after each command; pipelined branch sends three
  commands and reads three replies. In both branches the final read is what
  message() returns.
- SmtpClient::new sets pipelining: true, but the public new() docs list only
  "No authentication / No SMTPUTF8 support" as defaults; the pipelining
  default is undocumented. Whether GreenMail advertised PIPELINING was not
  recorded; the byte trace looks non-pipelined, but the shift argument applies
  to both branches.

Why the shift:

1. A cancelled send can leave (a) unread server replies and (b) bytes written
   into the shared BufStream but not flushed. Dropping the future does not drop
   the stream; the transport still owns it.
2. The survivor's send begins a new transaction but reads replies already
   queued. It is one reply behind per stale reply.
3. When message() reads the "final" reply it receives the 354 DATA prompt.
   read_response treats it as positive and message() returns Ok(354). The real
   final 250 remains queued.
4. This matches the observation: the survivor message is delivered (server
   accepted it after the terminator), while send returns 354. M1 sees the
   survivor message in every running plan.
5. At #0@3 the shift goes the other way: the survivor waits for a reply that
   never comes, and the stream has no read timeout, so only Dropwise's 20 s
   bound ends it.
6. At #0@4 the byte trace shows the target body had not reached the proxy when
   the target was cancelled, yet M1 Post sees the target message. The most
   consistent explanation is that the body/terminator were already in the
   shared BufStream and the survivor's send flushed them: the cancelled
   message was delivered after cancellation. This is inferred from counters
   plus observer, not a wire capture.

Is it a clear library bug?

- User-visible consequence: yes. send returns is_ok() true with code 354 (the
  server is asking for DATA, not confirming acceptance); the final 250 is
  unread; a third send would consume that stale 250. At one boundary the next
  send never returns. Reproduced deterministically.
- Public contract reasoning: send is documented as "Sends an email" and
  returns SmtpResult; an intermediate 3xx is not a completed send. There is no
  doc that Ok may be a non-final reply, no cancellation-safety statement, no
  poison/reset API, and no recovery except quit/drop/reconnect. A caller
  following the canonical example cannot distinguish accepted from DATA
  prompt. This is a contract gap at minimum.
- Patch/config contrast: not run. The parent stopped further runs before a
  dependency-copy patch. Likely minimal fixes: (a) in SmtpStream::message,
  require final 250/2xx and error otherwise; (b) in SmtpTransport::send, use a
  drop guard that poisons the transport if the future is dropped mid-send, so
  the next send errors instead of desynchronising. Either turns 354 into an
  error; (b) also turns #0@3 into an error. Caller reconnect is the simpler
  workaround, not tested.
- Classification: confirmed observation; public contract silent/ambiguous;
  pending upstream (minimal patch contrast not performed, no upstream issue
  filed). Strong candidate defect, not patch-confirmed.

## Docs / RSET / quit recovery check

- RSET: no public RSET, reset or reconnect on SmtpTransport or SmtpClient. A
  caller cannot reset the SMTP transaction through the typed handle.
- quit: public async quit() sends QUIT. It is the only transaction-ending
  method, but it is not a reset and was not tried after cancellation. After a
  desync it could itself wait for a stale reply; not tested.
- into_inner: public; returns the stream so the caller can discard it and
  reconnect. No built-in reconnection (SmtpTransport has no host address).
- get_mut/get_ref: expose the underlying stream; no documented recovery use.
- Caller-side stream timeouts are explicitly the caller's responsibility per
  issue #57; the canonical example has none.
- Recommendation: after a cancelled send, drop the transport and reconnect;
  do not reuse the same stream. A poison-on-cancel guard would make this
  failure loud instead of silent.

## Coverage

Four Pending boundaries reached, each with Immediate and AfterWake; 8 plans per
rep, all realized, exhaustive, no truncation. Survivor used the same
SmtpTransport object and same proxy connection (connections=1 in every
completed run; no reconnect/login/NOOP/reset). Library did not close or
reconnect; it has no address to reconnect to. Observer calibration is distinct
(1 vs 2 messages), fresh users per run, no Mixed/Neither/Unknown. B1 isolates
drop from reuse: drop is clean; reuse produces 354 and the hang.

## Cost

- Official run wall clock 03:06:24–03:13:04 = 6 min 40 s, dominated by 18 x
  20 s scenario timeouts (B2/B3/M1, 2 per rep x 3 reps each).
- 120 scenario setups across the 12 level reps plus prep; 248 GreenMail users
  created via REST; 12 LADDER lines; 96 planned cancellation trials plus
  baselines and M1 calibrations.
- Logs 276 KB, 15 status files; no external account; all SMTP/REST loopback.

## Limitations

- One client version, one server (GreenMail 2.1.14, auth disabled, no TLS), one
  message size, one proxy delay (50 ms).
- Pending is only server-to-client chunk delay; boundaries are read/write
  buffer boundaries, not a complete enumeration of source awaits. Exact wire
  sequence was not recorded; command mapping inferred.
- The canonical stream has no read timeout. The 20 s hang is bounded only by
  Dropwise. With a caller timeout it would be an I/O error; the false 354 is
  independent of the timeout.
- Observer cannot run for the two #0@3 timeout plans; delivery during hang is
  unknown; container removed so mailboxes are gone.
- M1 does not record survivor SmtpResult per plan; its 354 mapping comes from
  the same deterministic B2/B3 plans.
- Archived official B3 logs contain duplicated PHASE M1 survivor_start lines
  from a harness patch bug. B3 markers are correct; LADDER, B2/B3-RUN and
  M1-OBS data are unaffected. Source fixed after the run; official logs not
  regenerated.
- No dependency-copy patch, pipelining-off, large-body or third-send contrast
  (parent stopped further runs). Expect::Atomic accepts Pre and Post, so M1
  does not flag late delivery or 354 by design.

## Productization friction (for main agent)

- Settle::ZERO does not exist; task brief and API disagree. Constructed
  Settle { tokio_time: ZERO, real_time: ZERO, watchdog: 10 s }.
- explore accepts only Result<(), String> and Trial has no scenario observation
  field. B2/B3 require recording the concrete SmtpResult while checking only
  Ok; I printed per-run lines and parsed raw logs. A record-this-value hook or
  an explore variant with a non-compared Debug observation would remove this.
  LADDER counts cannot carry it.
- LADDER helper reports counts/violations, not observed values;
  Report::diagnostics shows values only for explore_observed.
- "Check only Ok" passes Ok(354) for an SMTP send. A final-response predicate
  (or returning Response and checking code == 250) would expose the false
  success at ladder level. Task deliberately asked only Ok; this is a scenario
  design lesson, not a change to original results.
- async-smtp docs: pipelining default true is undocumented; send has no
  cancellation/final-reply contract; Ok can be 354; canonical example has no
  stream timeout. Issue #57 puts timeouts/buffering on caller but does not
  cover false-Ok reply handling.
- Path: from clients/smtp/tests the ladder is four levels up
  (../../../../support/ladder.rs), not three; parent already corrected this.
- Flavor::MultiThread { workers: 1 } plus blocking ureq REST inside the
  scenario worked because the scenario is block_on-driven; usable for future
  real-client harnesses.

## Next concrete experiment

1. Wire capture (recording proxy or GreenMail verbose) to map each #0@N plan to
   exact command/reply offset; confirm extra-250 and #0@4 buffered-body
   hypothesis.
2. Read-timeout contrast: wrap stream/send in a timeout; show #0@3 becomes an
   I/O error while false 354 remains.
3. Dependency-copy patch: require final 250/2xx in message(); add poison-on-
   cancel guard. Run B2/M1; expect 354 -> error, #0@3 -> explicit error.
4. Recovery contrast: after cancellation, fresh reconnect vs quit vs
   into_inner/drop; record which restores 250 and which still hangs.
5. Config variation: pipelining(false) and a larger body forcing write-side
   Pending; label as added exploration, not original B2/B3.
6. Third send after Ok354: verify the predicted stale final 250 shifts the next
   transaction again; test a fourth send for recovery.

## Files

- SCENARIO.md — pre-registered scenario.
- tests/levels.rs — runner and all scenarios.
- run.sh — container lifecycle and log capture.
- environment.txt — versions/image/commit.
- logs/LADDER.txt — all LADDER and LADDER-VIOLATION lines.
- logs/SUMMARY.md — generated table and M1 per-plan map.
- logs/ and summarize.py — raw evidence and parser.

---

# Round 2 — codes in the observer, third send, trace, and fix contrast

Status: added 2026-10-21 after reading the round-1 result. This is still the
same SMTP client and service; it is not a new Phase-4 target. Original code and
logs are untouched. New logs are named logs/round2-*.

## What changed

1. M1R2: `explore_observed` with an Observation2 containing the concrete
   survivor and third `SendView` plus the REST messages. The target result is
   excluded (it is absent after a cut); a late target message remains in the
   REST list. The two post-target sends are bounded at 2 s so a hang is a value,
   not only a Dropwise liveness failure.
2. `round2-calibration`: explicit pre/post normal sends with three messages.
3. `round2-poison`: second and third sends on the old transport, then drop the
   transport, reconnect, send a recovery message, and read REST.
4. `round2-diagnose`: replays both #0@3 plans with `RUST_LOG=async_smtp=debug`.
5. `patches/async-smtp-poison.diff` and `run-round2-fixed.sh`: a disposable
   dependency-copy fix and the same poison/reconnect contrast.

Commands:

    bash run-round2.sh
    bash run-round2-fixed.sh

## Calibration

Three reps, identical:

| side | target | survivor | third | REST messages |
|---|---|---|---|---|
| pre  | not started | 250 | 250 | survivor, third (count 2) |
| post | 250 | 250 | 250 | target, survivor, third (count 3) |

Both sides have the same survivor/third codes; the baselines are distinguished
by the target's message. The observer is sensitive, and the two post-target
codes are stable at 250 before any cancellation.

## M1R2 official, three reps

Each rep has 8 plans (4 Pending boundaries x 2 races), all realized, no
unrealized, no baseline_errors. Every rep:

    plans=8 violations=8 unrealized=0 unsettled=0 baseline_errors=0 exhaustive=true
    pre=0 post=0 mixed=0 neither=8 unknown=0

| plan (both races) | survivor | third | REST messages | classification |
|---|---|---|---|---|
| #0@1 | 354 | 354 | survivor, third | effect correct, response wrong |
| #0@2 | 354 | 354 | survivor, third | effect correct, response wrong |
| #0@3 | 2 s timeout | 2 s timeout | none | no effect, no response |
| #0@4 | 354 | 354 | target, survivor, third | effect correct, response wrong |

All three reps are identical. Across 3 reps: 24 plan violations = 18 false-354
plans + 6 timeout plans. User-visible phenomena: 18 false-success responses
(message delivered, code is the intermediate 354) and 6 hangs. There are 0
plans where send claimed a completed success and the corresponding message was
absent. At #0@4 the target was delivered late but had no target result; that is
not a claim of success by the target.

This means the round-1 M1 2/8 result was not a contradiction: that observation
only carried message count/content, so it could not see 354. The new 8/8 is the
same execution with the return codes included; it is a stronger oracle, not a
retest that overturns the old observation.

## Third send

When the second send returns 354, the third also returns 354, every time, in
every one of the 18 false-354 plans. The third message is delivered, but its
final reply is still not 250. The stale final 250 is not consumed by the third
transaction; the shift persists and does not self-heal. At #0@3 both the second
and third time out at 2 s.

Round-1 prediction corrected: a third send does not show a stale 250 or a
different result; it reproduces the same intermediate response.

## #0@3 command/reply trace

The two #0@3 replays were realized. The debug trace for one of them (token and
timestamps omitted) is:

    target:   MAIL -> 250; RCPT -> 250; >> DATA
    [target cancelled before reading 354]
    survivor: >> MAIL; << 354 (the target's DATA prompt)
    survivor: >> RCPT; no reply (server is in DATA/body mode)
    third:    >> MAIL; no reply (still DATA body)

The cut is after DATA was written, before its 354 was consumed. The server is
in DATA mode; MAIL/RCPT are consumed as body bytes and produce no command reply.
The canonical transport has no read timeout, so the hang is bounded only by the
2 s experiment wrapper or Dropwise's 20 s. The old byte counter at #0@3 was 94
up and did not include the DATA write; this is why the Pending number alone was
not a protocol stage.

The #0@4 late target delivery is visible in REST but was not newly wire-traced;
the buffered-body explanation remains an inference.

## Old transport vs fresh reconnect

Original dependency, 3 reps, all 24 cancelled plans:

| plan | second on old | third on old | old-transport effects | fresh recovery |
|---|---|---|---|---|
| #0@1, #0@2 | 354 | 354 | survivor + third | 250, delivered |
| #0@3 | 2 s timeout | 2 s timeout | none | 250, delivered |
| #0@4 | 354 | 354 | target + survivor + third | 250, delivered |

The old transport never recovered: the third send is still 354 and delivery of
survivor/third continues. Dropping it and connecting a new transport gave 250
and delivered the recovery message in all 24 plans. This is a clean
drop-and-reconnect workaround; quit/RSET were not tested.

## Minimal dependency-copy fix

`patches/async-smtp-poison.diff` adds a `poisoned` flag to SmtpTransport:

- `send` returns an explicit client error if poisoned;
- sets poisoned before the transaction;
- clears it only after a complete 2xx final reply;
- returns an error for a non-final reply instead of returning Ok(354).

A dropped send leaves the flag set. `run-round2-fixed.sh` copies async-smtp
0.10.2 to /tmp, applies the patch, builds a separate manifest, and runs the
calibration plus poison contrast. The registry and repository copies are not
modified.

Fixed results, 3 reps:

- normal calibration remains 250/250, pre count 2, post count 3;
- all 24 cancelled plans: second and third return `Error("client: previous send
  did not complete; reconnect the transport")`; no 354, no 2 s timeout, no
  survivor/third delivery;
- fresh transport recovery is 250 and delivered in all 24;
- #0@1/#0@2/#0@3 have only the recovery message (count 1); #0@4 has target +
  recovery (count 2), because the target body was already in flight.

This confirms that a missing cancellation guard is sufficient to explain the
silent 354 and the DATA-mode hang. It is a minimal safety guard, not an in-place
recovery protocol and not an upstream patch. It intentionally makes the old
handle unusable; the caller must reconnect.

## Classification and upstream status

- Confirmed user-visible defect in the client: `SmtpTransport::send` can return
  Ok(354), an intermediate DATA prompt, after a cancelled send; it can also
  leave the next transaction waiting forever in DATA mode. Normal sends return
  250, so this is not a server-wide failure.
- Confirmed: the state does not self-heal after a third send; only a new
  transport recovers in this experiment.
- Public docs/API have no cancellation, poisoning or final-reply contract;
  `read_response` accepts any positive reply and `message` returns it. This is a
  defect relative to the meaning of send/SmtpResult, but it is not a violation
  of an explicit written cancellation promise.
- Upstream status: no issue/PR/comment was filed. There is no maintainer
  acknowledgement and no upstream fix. The local patch is only a
  dependency-copy counterexample.

## Corrections to the round-1 fragment

1. "M1 cannot see the mismatch" is too strong: the round-1 M1 shape did not
   carry return codes; the new observation does. M1R2 is 8/8, not 2/8, because
   the stronger observation sees the 354 and the timeout.
2. The predicted stale 250 on the third send is not observed: third sends also
   return 354.
3. The #0@3 stall is now traced: cancellation after DATA write, before 354; the
   survivor is in DATA mode. The earlier byte-count mapping (94 up = MAIL+RCPT)
   was incomplete.
4. Patch contrast and fresh-transport recovery now exist; the old "not run"
   limitation is closed.
5. `Settle::ZERO` is not a Dropwise product defect; it is simply not a public
   constant. The scenario constructs the two zero durations explicitly.

## Coverage and limits

- One client version, one server, no TLS/auth, one proxy delay, one message
  size. The 2 s bound is an experiment choice; it demonstrates the hang but is
  not the 20 s product behavior.
- No fixed M1R2 double-baseline: the fixed behavior deliberately refuses the
  survivor send, so poison/reconnect is the appropriate oracle. No rejected
  send, quit/RSET, pipelining(false) or large-body test.
- #0@4 target delivery remains observer-plus-inference, not a wire capture.
- The 24 plan violations are three reps of the same 8 plans, not 24 independent
  root causes. The phenomenon count is 18 false-354 executions and 6 timeout
  executions; the underlying cause is one unprotected shared transport state.

## New artifacts

- `run-round2.sh`, `run-round2-fixed.sh`, `patches/async-smtp-poison.diff`
- `logs/round2-*`, `environment-round2.txt`, `environment-round2-fixed.txt`
- `tests/levels.rs` round-2 commands; original scenarios unchanged
- `logs/round2-SUMMARY.txt`; this addendum



## 收尾复核

2026-10-21 的独立审查、runner 修正和补跑结果见 [总报告 §9](../../RESULTS.md#9-收尾复核2026-10-21)。
本片段此前的数字是第二轮历史结果，不替代最新的补丁边界和验证结论。
