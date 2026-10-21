# SCENARIO: async-imap 0.12.0, direct handle, SELECT INBOX

Status: **unfrozen exploration, round 1**. Written before the first cancellation
run (2026-10-21). This file is not a Phase 4 preregistration and does not change
the Phase 4 list, gates or budget.

## Question

A caller holds one authenticated `async_imap::Session<tokio::net::TcpStream>`
outside the target future and sends `SELECT INBOX` through a 50 ms reply-delay
proxy. The target future is dropped after an observed `Pending`. On the **same**
session object, with no close/reconnect/login/reset/re-select, the survivor then
appends a unique message to `INBOX` and reads it back. What does the session and
the server observe? This is an exploration of one direct-handle boundary, not a
proof about async-imap.

## Fixed pieces (shared by B1/B2/B3/M1)

- client: `async-imap =0.12.0` (features `runtime-tokio`, default features off),
  `tokio 1.x` multi-thread runtime.
- server: `greenmail/standalone:2.1.14`, one disposable container per runner,
  loopback-only random host ports. Users are explicit
  (`imapuser:pass@example.com`); the mailbox exists before the session.
- observer: GreenMail REST
  `GET /api/user/imapuser@example.com/messages` direct to the container's HTTP
  port. It bypasses async-imap and the proxy. A raw SMTP connection (also
  independent of the session and proxy) seeds one unique message in setup.
- handle: `Box<Session<TcpStream>>` created in setup inside the scenario, outside
  the target future. The target future only borrows it. The box gives a stable
  object address across awaits.
- target: `ctx.target(session.select("INBOX"))` (one session-state round trip).
- Pending: `dropwise::net::Proxy::start_with(imap_addr, ProxyOptions { reply_delay:
  50 ms })`, which holds every server->client chunk for real time. The proxy is
  the only path to IMAP; the observer and SMTP seed do not use it.
- config: `Flavor::MultiThread { workers: 1 }`, `races = [Immediate, AfterWake]`,
  `max_cancellations = 1`, `max_runs = 64`, `scenario_timeout = 20 s`,
  `Settle { tokio_time: ZERO, real_time: ZERO }`.
- levels share setup/target/Pending/Config and differ only after cancellation:
  - B1 (`explore`): cancel, then stop. Judge only panic/timeout/obligation leak.
  - B2 (`explore`): retry `select("INBOX")` exactly, same session, bounded 5 s;
    judge only `Ok`, record the actual `Mailbox` fields or error.
  - B3 (`explore`): append a different unique message `s-<run>-b3` to INBOX on
    the same session; judge only `Ok`, record the actual result and the REST
    message count for that subject for diagnosis.
  - M1 (`explore_observed`, `Expect::Atomic`): append unique
    `s-<run>-survivor`, then `SEARCH SUBJECT` and `FETCH BODY.PEEK[TEXT]` on the
    same session; independent REST observer returns the current run's
    normalized subjects and bodies. Obs contains concrete values only; the
    per-run id is normalized to `RUN`, and absolute mailbox sizes/seqs/UIDs are
    excluded because they grow across runs.

M1 pre is expected to be observably different from M1 post: if SELECT was never
sent, the mailbox is not selected and SEARCH/FETCH should fail, while APPEND and
the independent observer still work. `Expect::Atomic` allows both.

## Before the ladder

1. Normal no-cancel run: SELECT + APPEND + SEARCH/FETCH + REST observer, with the
   same proxy and 50 ms delay.
2. Reuse/identity diagnostic: three sequential commands on one session;
   session-box pointer, stream pointer, and proxy connection count/byte counters
   before and after. Same pointer plus one proxy connection and growing byte
   counters is evidence of one logical handle on one TCP connection; a changed
   count is a reconnect.
3. Calibration check: manual pre (no SELECT) and post (SELECT completed) paths;
   both must complete and the REST observer must see the seed and the appended
   message.

Only after these pass do B1/B2/B3/M1 mean anything.

## Detection rule

Before the first cancellation run, only public async-imap docs (docs.rs 0.12.0),
README/changelog/API signatures and server docs are read. No async-imap
cancellation/reset implementation is read. After a result looks like a
violation, source and upstream issues may be read for triage, and that is
recorded separately in RESULTS-FRAGMENT.md.

## Raw logs

`run.sh` starts the container, writes `logs/container.txt` and
`environment.txt`, runs `preflight` once and then `ladder 1|2|3` in separate
processes. Raw stdout/stderr go to `logs/preflight.*` and `logs/round-N.*`.
A container startup log is kept in `logs/greenmail-startup.log`.

## Addendum after the first cancellation run (2026-10-21)

This addendum records changes made **after** the first ladder run. The B1/B2/B3/M1
operations, target, proxy, Config, normalization and judgement rules above were
not changed. Additions:

- A `probe` phase was added to the same binary, after the result looked like a
  failure. It uses only the public async-imap API: `tokio::time::timeout(40 ms,
  session.select("INBOX"))` drops the SELECT future (the same drop that
  `ctx.target` performs), waits 150 ms so the proxy delivers the abandoned
  reply, then either (A) calls APPEND immediately, or (B) reads the queued
  responses with `session.read_response()` until the SELECT tagged Done and then
  calls APPEND. It never changes the ladder. Output: `logs/probe.stdout.log`.
- After the first cancellation run, the async-imap source was read for triage:
  `client.rs` (append, select, run/check response), `parse.rs`
  (`parse_mailbox`, `parse_ids`, `filter`) and `check_done_ok_from`. Upstream
  issues #63/#64 (command timeouts), #108/#109 (APPEND response parsing) were
  read. A later, focused search found PR #142 (merged 2026-09-11): it makes
  LOGIN ignore a `NO` completion for another request, and its body explicitly
  mentions a dropped response-reading future; 0.12.0 includes it. No general
  cancellation contract or APPEND equivalent was found. All of this is after
  the fact, not part of the detection input.
- A dependency-copy contrast was run under `/tmp` only: a copy of async-imap
  0.12.0 whose APPEND continuation read skips non-`Continue` responses. No file
  in the repository or in the repo's Cargo dependency graph was patched. Output:
  `logs/patch-contrast.txt`. This is triage, not the official ladder.
- `Settle::ZERO` does not exist in the public API. The experiment uses
  `Settle { tokio_time: Duration::ZERO, ..Settle::default() }`; the default
  `real_time` is already zero. The effective settling is zero.
- The first ladder was also run against a throwaway probe container before the
  clean runner. Only `run.sh` output in `logs/` is reported; the clean run
  used a fresh container.
- The manual pre calibration showed `search = Ok(0)`, not an error. GreenMail's
  log shows that every unselected SEARCH was rejected with BAD in
  AUTHENTICATED state. async-imap returned `Ok(empty)` anyway. This is recorded
  as an adjacent, non-cancellation finding in RESULTS-FRAGMENT.md; it does not
  change the pre/post distinction (the observer still saw seed + survivor in
  pre), but the `Ok(0)` field is a swallowed server error, not a genuine
  zero-match result.

---

## Round 2 (2026-10-21): recovery order, APPEND patch, rerun

Status: **round-2 expansion**. This section was written after the round-2 run,
but the phases, commands, bounds and reading rules below were fixed before it.
The round-1 text above is unchanged; round-2 counts are separate.

### Added questions

1. After a realised cancelled SELECT, does a retry SELECT on the same Session
   actually restore it for APPEND -> SEARCH/FETCH? Record the retry result, the
   returned Mailbox flags and permanent_flags before/after, APPEND raw reply,
   SEARCH/FETCH, REST observer count/subjects/bodies, proxy connection count and
   session/stream identity. Run retry-first and APPEND-first (reverse) three
   times each.
2. Is the round-1 APPEND-continuation sketch valid against normal APPEND,
   server NO/BAD, EOF/close, stale tagged/untagged SELECT responses and a stale
   continuation? Add tests that fail on published 0.12.0 and pass on the patch.
3. Re-run B1/B2/B3/M1 three times with published and patched async-imap. Does
   the patch remove B3/M1 APPEND failures, and does B2 flag duplication remain?
4. Reproduce SEARCH returning Ok(0) while GreenMail rejects it as BAD, with no
   cancellation and no broad scan.

### Added harness and commands

The round-2 binary is [round2/src/main.rs](round2/src/main.rs). Phases:

- preflight: one no-cancel SELECT -> APPEND -> SEARCH/FETCH -> REST path through
  the same 50 ms proxy; prints identity and observer values.
- chain N: retry SELECT (5 s bound) -> APPEND -> SEARCH/FETCH -> observer.
- chain-reverse N: APPEND -> retry SELECT (2 s bound) -> SEARCH/FETCH ->
  observer. The 2 s bound makes a desynchronised connection record Timeout
  rather than waiting for the 5 s per-op bound.
- ladder N: B1/B2/B3/M1 as in round 1, with round-2 subject names.
- patch-behavior: normal selected/unselected APPEND, nonexistent mailbox (NO
  after +), invalid flags and invalid date (BAD before +), stale SELECT then
  APPEND, stale SELECT then rejected APPEND, and three fake-stream modes (EOF,
  own-tag NO before +, stale +).
- search-unselected: SEARCH before SELECT, SELECT, SEARCH after SELECT, with the
  GreenMail log line delta captured by the runner.

Commands, from the repository root:

    bash prospective/phase4-exploration/clients/imap/run-round2.sh
    bash prospective/phase4-exploration/clients/imap/patched/run-round2-patch-tests.sh

`run-round2.sh` builds the published crate, copies the crate and dependency to
/tmp, applies
[patched/append-continuation.round2.patch](patched/append-continuation.round2.patch),
builds the patched binary, and starts a fresh container for each variant. It
writes only logs/round2-* and /tmp paths. Both containers are stopped and
removed by the runner; no other container is touched.

### Environment

- greenmail/standalone:2.1.14, digest
  sha256:1ef95a966418cd09b7ea91d504d8c0826bbe7a2f6e679a75c601a831587c1626,
  two disposable containers (one published, one patched), loopback-only random
  ports.
- async-imap 0.12.0, runtime-tokio, default features off; tokio 1.x
  MultiThread workers=1; races [Immediate, AfterWake], max_cancellations=1,
  max_runs=64, scenario_timeout=20 s, zero settling; 50 ms reply-delay proxy.
- rustc 1.98.0, cargo 1.98.0, rustfmt 1.9.0-stable, clippy 0.1.98.
- Exact container IDs, ports, commands and build identity are in
  [logs/round2-environment.txt](logs/round2-environment.txt) and
  [logs/round2-commands.txt](logs/round2-commands.txt).

### Reading rules for round 2

- The round-2 ladder is a new table. Its 46 plans / 22 violations / 7
  unrealised (published) and 46 / 0 / 11 (patched) are not added to the
  round-1 totals.
- A plan is a generated cancellation plan; an unrealised plan did not reach the
  planned Pending and is not evidence of an effect. Report realised cuts
  separately.
- Actual plan counts differ by repetition (for example B2 2 or 4, B3 4 or 2).
  This is real scheduling variance, not a changed operation; the tables report
  the actual count, not a target of four.
- The retry-first chain and the reverse chain are different experiments. A
  retry SELECT before APPEND can resynchronise the command stream; a retry
  SELECT after a failed APPEND is itself consumed as literal data.
- Duplicated flags are recorded as values, not counted as a B2 violation (B2
  judged only Ok). The round-2 transcript attributes them to client
  accumulation in parse_mailbox, not to the server.
- The APPEND patch is a dependency-copy triage change, not an upstream fix. Its
  known limitation is a stale + continuation, which is untagged and cannot be
  correlated to a command; both builds time out in that synthetic case.

### Raw round-2 outputs

- logs/round2-unpatched-*.stdout.log, logs/round2-patched-*.stdout.log:
  preflight, three chain reps, three reverse reps, patch-behavior,
  search-unselected, three ladder reps; stderr files are separate.
- logs/round2-greenmail-unpatched.log, logs/round2-greenmail-patched.log:
  container logs.
- logs/round2-patch-tests-*.log and logs/round2-patch-*.log: dependency-copy
  tests, fmt and clippy.
- logs/round2-summary.txt: consolidated numbers.
- logs/round2-pr142.diff: fetched PR #142 diff for the tag-first comparison.

The full round-2 write-up and limitations are appended to
[RESULTS-FRAGMENT.md](RESULTS-FRAGMENT.md), after the unchanged round-1 text.
