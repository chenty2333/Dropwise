# SMTP direct-handle cancellation scenario (async-smtp 0.10.2)

Status: exploration, written before the official three-repetition ladder
(2026-10-21). This is not the frozen Phase 4 study.

## Question

A real SMTP transport is connected and is not moved into the target future.
One complete `send` is cancelled by dropping its future. The survivor does
**not** close, reconnect, log in, send NOOP or reset: it sends again on the same
`SmtpTransport` object over the same TCP connection. Does the transport survive
the cancellation, what does the next send return, and what does an independent
observer see?

The forbidden recovery is deliberate. The experiment measures the public
handle's post-cancellation state, not a caller-written recovery protocol.

## Client and service

- `async-smtp = "=0.10.2"`, default features (`runtime-tokio`, `tokio`).
- Public API from docs.rs 0.10.2 and the crate's published `examples/send.rs`:
  ```rust
  let stream = tokio::io::BufStream::new(TcpStream::connect(addr).await?);
  let mut transport = SmtpTransport::new(SmtpClient::new(), stream).await?;
  let result = transport.send(SendableEmail::new(envelope, raw_message)).await;
  ```
  `send(&mut self, SendableEmail) -> SmtpResult` borrows the handle; it does not
  consume it. The transport is created outside the target future.
- GreenMail `greenmail/standalone:2.1.14`, one disposable container per runner,
  SMTP and REST published on random `127.0.0.1` ports.
- GreenMail users are explicit. Each scenario run creates two fresh users via
  REST (`<token>-a@example.com`, `<token>-b@example.com`) so a mailbox cannot
  inherit a message from an earlier run or calibration.
- Independent observer: `GET /api/user/<user>/messages`, reading `uid`,
  `Message-ID`, `subject`, `mimeMessage`. It bypasses async-smtp and the proxy.

## Pending mechanism

`dropwise::net::Proxy` forwards `127.0.0.1:<proxy>` to the GreenMail SMTP port
with `ProxyOptions { reply_delay: 50 ms }`. Every server-to-client chunk is held
50 ms real time, so a send's reply reads return `Pending`. The client-to-server
direction is not delayed. No server behaviour is changed by the proxy.

The direct handle and proxy are created inside the scenario (fresh per run) but
before `ctx.target`; only a mutable borrow of the transport enters the target
future.

## Config (shared by B1, B2, B3, M1)

```rust
Config {
    max_runs: 64,
    max_cancellations: 1,
    races: vec![Race::Immediate, Race::AfterWake],
    flavor: Flavor::MultiThread { workers: 1 },
    settle: Settle { tokio_time: Duration::ZERO, real_time: Duration::ZERO,
                     watchdog: Duration::from_secs(10) },
    scenario_timeout: Duration::from_secs(20),
    repeat_at_site: false,
    gap_choices: Vec::new(),
}
```

`Settle::ZERO` is not a constant in the public Dropwise 0.1.0 API; the two time
fields are set to zero explicitly. The settle watchdog is only a bound on a
stuck settle phase.

## Target and messages

Target: one complete `transport.send(...)` of a raw RFC 5322 message with unique
subject, body and `Message-ID` for the run, to the run's user A.

Survivors:

- B1: none. Cancel, then return; judge only panic, timeout and obligation leak.
- B2: send the same message again (same envelope, same raw bytes including the
  same `Message-ID`) on the same transport; check only that `send` returns
  `Ok`, and record the concrete `Response` code/messages or error.
- B3: send a different message of the same kind (different subject, body,
  `Message-ID` and recipient: user B) on the same transport; check only `Ok`,
  and record the concrete value/error.
- M1: `explore_observed(Expect::Atomic)`. Survivor sends a unique second
  message to user A. The REST observer returns message count, subjects, bodies
  and Message-IDs. The per-run token is normalised to `<run>` in those fields,
  so calibration samples are comparable and the observation is concrete and
  `PartialEq`.

## Levels and repetitions

Four levels, three repetitions each, same setup, target, Pending mechanism and
`Config`:

| level | after the cancelled target | oracle |
|---|---|---|
| B1 | stop | `explore`: no panic, no timeout, no obligation leak |
| B2 | same send, same arguments | `explore`: survivor `Ok`; record actual value |
| B3 | different send, same kind | `explore`: survivor `Ok`; record actual value |
| M1 | unique send + REST observer | `explore_observed(Expect::Atomic)` |

Each level prints the shared `LADDER` line via
`prospective/support/ladder.rs` (included read-only through
`#[path = "../../../../support/ladder.rs"]` from `tests/levels.rs`) plus the
Dropwise diagnostics and per-run concrete values.

## Pre-ladder evidence (must exist before interpreting cancellation)

1. `nocancel`: two normal sends on one transport, then REST observer sees both.
2. `reuse`: two consecutive sends, proxy connection count and client address
   before/after; report whether one TCP connection was used. Also prints the
   transport pointer before/after.
3. `calibration`: a manual pre (target never started) and post (target then
   survivor) run, each observed through REST; pre and post must be different and
   must show the expected 1 vs 2 messages.

## Interpretation

- A plan is a Pending boundary of the marked future under the observed schedule.
  Real I/O can move boundaries; unrealized plans are reported, not retried.
- M1 pre means the cancelled target left no message; post means the target's
  message was accepted. Under `Expect::Atomic`, either is allowed. Mixed,
  Neither and errors are violations.
- B1 clean is not evidence that reuse is safe. B2/B3 clean means only that the
  next send returned `Ok`; the actual `Response` code is recorded because an
  intermediate SMTP reply can also be "positive".
- A hang in the survivor is a liveness result caused by the survivor, not by
  dropping the target; the raw log has `PHASE ... survivor_start` with no
  `survivor_done` for that run.
- The first cancellation runs happened before any async-smtp implementation
  source was read. After a violation-shaped result, source and upstream issues
  were used only to triage.

## Running

```sh
bash prospective/phase4-exploration/clients/smtp/run.sh
```

Optional environment: `LEVELS="B2 M1"`, `REPS="1"`, `RUN_PREP=0`.
Raw stdout/stderr is written to `logs/`; `environment.txt` records versions,
image id/digest, rustc and the Dropwise commit. The container is removed by the
trap on exit. Nothing outside this directory is modified.

## Round 2 addendum (2026-10-21, after the round-1 fragment)

Round 1 showed that the observer saw the survivor mail while M1 did not carry
the send code into the observation. It therefore could not distinguish
"effect correct, response wrong" from "response claims success, effect absent".
Round 2 keeps the original four-level code and logs intact and adds a separate
bounded M1 variant, a third send, a #0@3 command/reply trace, and a
drop-and-reconnect contrast.

New commands:

    bash run-round2.sh          # original dependency, 3 reps of M1R2 + poison
    bash run-round2-fixed.sh    # patched dependency copy, 3 reps of poison

`round2-calibration` is the explicit pre/post evidence. `round2-m1` runs
`explore_observed` with the new Observation2. `round2-diagnose` replays the two
#0@3 plans with async-smtp debug logging. `round2-poison` sends a second and a
third message on the old transport, drops it, reconnects, and sends a recovery
message.

### M1R2 observation

Observation2 has three semantic fields: `survivor: SendView`, `third: SendView`
and the REST `messages: Vec<MessageView>`. `SendView` is `Code(u16)`, `Timeout`
or `Error(String)`; no Debug string is used as the compared value. The target
result is not in the observation: after a cut it is absent, and the contract
compares the survivor's effects. A late target message is still visible in the
REST list.

The two sends after the target are bounded at 2 s each. This is specific to the
round-2 experiment; the original B2/B3/M1 scenarios remain unbounded and keep
the 20 s Dropwise liveness result. Bounding makes the #0@3 hang visible as a
`Timeout` value and allows the third send to run.

Calibration (3 reps, identical): pre has survivor 250, third 250 and exactly
two messages (survivor, third); post has target 250, survivor 250, third 250 and
exactly three messages (target, survivor, third). The two baselines are
distinguishable by the target message, not by the surviving send codes.

| plan (both races) | survivor | third | REST messages | classification |
|---|---|---|---|---|
| #0@1 | 354 | 354 | survivor, third (2) | effect correct, response wrong |
| #0@2 | 354 | 354 | survivor, third (2) | effect correct, response wrong |
| #0@3 | 2 s timeout | 2 s timeout | none (0) | no effect, no response |
| #0@4 | 354 | 354 | target, survivor, third (3) | effect correct, response wrong |

All eight plans are identical across the three reps: 8/8 Neither each rep, 0
pre, 0 post, 0 mixed, 0 unknown. Across three reps that is 24 plan violations:
18 false-354 plans and 6 timeout plans. There are zero plans where send claimed
success but the corresponding message was absent: in the 18 false-354 plans the
survivor and third messages are both present, and at #0@4 the late target
message is present as well. The target result is absent, as it must be after a
cut.

### Third send

After the second send returns 354, the third send also returns 354 in every
case. The third message is delivered (at #1/#2/#4), but its final reply is not
250. The stale 250 is not consumed by the third transaction and the shift does
not self-heal. At #0@3 both the second and third sends wait 2 s and time out.
This corrects the round-1 expectation that a third send might consume the stale
250 and show a different result.

### #0@3 trace

`RUST_LOG=async_smtp=debug` on a realized #0@3 replay gives this command/reply
sequence (timestamps and token omitted):

    target:   MAIL -> 250; RCPT -> 250; DATA (write, no 354 read yet)
    [target cancelled]
    survivor: MAIL -> 354 (the target's DATA prompt is read as the MAIL reply)
    survivor: RCPT -> no reply (server is in DATA/body mode)
    third:    MAIL -> no reply (still consumed as DATA body)

The cut is after the DATA command was written but before its 354 was consumed.
The server is in DATA mode; the survivor's MAIL/RCPT (and the third's MAIL) are
treated as message-body bytes and get no command reply. There is no read
timeout in the canonical transport, so the only bound is the 2 s wrapper here /
the 20 s Dropwise liveness timeout in round 1. The old byte counters at #0@3
(94 up) did not include the DATA write; this trace is why the Pending number
alone is not a stage.

The #0@4 late target delivery is observed in REST but was not newly traced at
the wire level. The round-1 explanation (body buffered and flushed by the next
send) remains an inference, not a new fact.

### Old transport vs reconnect

`round2-poison` cancels the target, sends second and third on the old transport,
then drops it and creates a new `SmtpTransport` to the same proxy. Original
dependency, 3 reps, all 24 cancelled plans:

- #0@1/#0@2: 354, 354; old transport remains shifted; messages survivor and
  third; reconnect send 250.
- #0@3: 2 s timeout, 2 s timeout; no survivor/third message; reconnect 250.
- #0@4: 354, 354; target, survivor and third messages present; reconnect 250.

In every plan the fresh transport returned 250 and the recovery message was
delivered. A third send on the old transport did not recover. `quit`/RSET were
not tested as recovery: dropping the old transport is the tested route.

### Minimal dependency-copy fix

`patches/async-smtp-poison.diff` adds a `poisoned` flag to SmtpTransport. `send`
checks it at entry, sets it before the transaction, clears it only after a
complete 2xx final reply, and returns an explicit client error on a non-final
reply. A dropped send leaves the flag set. The patch is applied to a disposable
copy of async-smtp 0.10.2 by `run-round2-fixed.sh`; no registry/repository
source is modified.

Fixed calibration: normal sends still 250/250, pre 2 messages, post 3.
Fixed poison, 3 reps, all 24 cancelled plans: second and third return
`Error("client: previous send did not complete; reconnect the transport")`; no
354, no hang, no second/third delivery. Reconnect recovers 250 in all plans.
At #0@1/#0@2/#0@3 the only message is the recovery message (count 1); at #0@4
the target message is also present (count 2: target, recovery).

This is a minimal safety guard, not an in-place recovery protocol and not an
upstream patch. It deliberately makes any later send on the old handle fail;
the caller must reconnect. The fixed M1R2 double-baseline was not used as the
oracle because it expects the surviving send to complete; the poison/reconnect
contrast is the right test for this semantics.

### Round-2 interpretation

- Confirmed user-visible defect: after a cancelled send, `SmtpTransport::send`
  can return `Ok(354)` for a message the server has accepted and can leave the
  next transaction hanging in DATA mode. The third send shows the state does
  not self-repair.
- The public docs still do not state a cancellation contract, so this is a
  defect relative to the meaning of `send`/`SmtpResult`, not a violation of an
  explicit written cancellation promise. Upstream status is separate and was
  not changed by this experiment.
- The minimal fix confirms the missing protection is sufficient to stop the
  silent 354 and the #0@3 hang, at the cost of requiring reconnect.
- Not tested: rejected-send behavior on the patched transport, `quit`/RSET
  recovery, pipelining(false), larger bodies, TLS, another server.
