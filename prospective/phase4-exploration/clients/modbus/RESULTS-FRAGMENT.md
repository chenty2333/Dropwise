# tokio-modbus 0.17.0 — results fragment (round 3, unfrozen exploration)

Not Phase 4 data. One target, one candidate root cause. No commit, no upstream
contact.

## Versions / environment

| item | value |
|---|---|
| client | `tokio-modbus` 0.17.0 (crate source, direct `client::Context`) |
| tokio | 1.54.0 |
| dropwise | path `../../../../../..` |
| server | `python3` 3.14.8 asyncio Modbus TCP, public MBAP, funcs 3/6/16 |
| server delay | 0 ms (pending is the proxy only) |
| proxy | `dropwise::net::Proxy`, `reply_delay = 50 ms`, loopback ephemeral |
| server port | 127.0.0.1:35031 (ephemeral, run-specific) |
| config | MultiThread(1), races Immediate+AfterWake, max_cancellations=1, max_runs=64, scenario_timeout=20 s, settle 0/0 watchdog 10 s |
| run | `logs/run.j6qLo7/` (`environment.txt`, timings, all stdout/stderr) |

## Preflight (uncancelled, one handle, independent observer)

```text
proxy_connections=1 (same client 57748; to_upstream 12 -> 48, to_client 11 -> 49)
PREFLIGHT write=Ok(()) observer=[100, 1] own=[100, 1] second=[100, 1] proxy_connections=1
PREFLIGHT-OK observer_effect=register1:0->1 same_handle=reused proxy_connections=1
```

- The uncancelled target changes the server register and the independent
  observer sees it (`[100,1]`), the same handle reads it back, and a second
  request on the same handle still sees one proxy connection (no reconnect).
- Manual calibration, 2 samples per side, identical per side:
  - pre ×2: `before=[100,0] write=Ok own=[100,7] visible=[100,7]`
  - post ×2: `before=[100,1] write=Ok own=[100,7] visible=[100,7]`
  - `pre != post` only in `before`, which is exactly the target's server-side
    effect.
- Server request log shows the setup reset (func 16, txid 0), warm-up read
  (func 3, txid 0), target write (func 6, txid 1, applied 1) and observer reads
  in order. 372 request lines total.

## Ladder (3 repetitions, identical counts)

| level | plans | violations | unrealized | unsettled | baseline_errors | exhaustive |
|---|---:|---:|---:|---:|---:|---|
| B1 WRITE_REG | 2 | 0 | 0 | 0 | 0 | true |
| B2 WRITE_REG | 2 | 2 | 0 | 0 | 0 | true |
| B3 READ_REGS | 2 | 2 | 0 | 0 | 0 | true |
| M1 WRITE_REG | 2 | 2 | 0 | 0 | 0 | true |

M1 verdict counts: pre=0 post=0 mixed=0 neither=2 unknown=0 (each rep).

Every ladder run has `baseline_targets=1`; the cancelled plans are
`#0@1:Immediate` and `#0@1:AfterWake`. There is one call site, so
`repeat_at_site=false` has no additional plans.

### Concrete counterexample (B2/B3/M1, 3/3 reps)

Server applied the cancelled write before the cut (observer `before=[100,1]`),
but the 50 ms proxy still held its reply when the target future was dropped.
The next request on the same `Context` reads that stale ADU:

```text
B2 retry  -> Err("mismatching headers: expected/request = Header { transaction_id: 2, unit_id: 255 },
                   actual/response = Header { transaction_id: 1, ... } Ok(WriteSingleRegister(1, 1))")
B3 read   -> the same mismatch against the same stale WriteSingleRegister reply
M1 before = Ok([100, 1])
   write  = Err(same header mismatch)         # yet the server applied reg1=7
   own    = Err(expected txid 3, got txid 2)  # the survivor write's reply is now stale too
   visible= Ok([100, 7])                      # independent observer: the "failed" write took effect
```

`visible=[100,7]` is the independent proof that the survivor write reached and
changed the server even though the client reported an error for it. In one
rep-3 M1 plan `before=[100,0]`: the observer read raced ahead of the cancelled
request's server processing; the stale reply still arrived and `write` failed
identically, so the verdict stays `Neither`.

Old vs new response: the stale frame is the *cancelled target's* reply
(function 6, txid 1, value 1). The survivor's response (txid 2 / txid 3) is
never matched; each subsequent reply shifts by one, so the connection stays
desynchronised until reconnect. `own` reading txid 2 (the survivor write reply)
confirms the shift rather than a single bad frame.

## Triage

1. Harness exclusion: preflight and all four baseline (uncancelled) runs are
   clean; calibration pre/post are stable and differ; the server log confirms
   the requests were served, so this is not a missing server effect or a
   harness timeout.
2. Source trace (tokio-modbus 0.17.0, `src/service/tcp.rs`): `call()` clears the
   read buffer, sends the ADU, then `framed.next().await` reads the next ADU
   and `verify_response_header` compares unit id and transaction id. A dropped
   `call()` future leaves its already-sent request's reply in the framed
   stream; the next `call()` consumes it and returns
   `ProtocolError::HeaderMismatch`. There is no public cancellation, drain or
   reset method on `Context`; `disconnect()` closes the socket.
3. Classification: **confirmed client-side response-association defect** for
   the cancellation-reuse pattern (concrete user-visible consequence: a write
   applied on the server but reported as an error, and every later request on
   the handle failing). It is **contract-external**: no docs promise reuse
   after dropping a request future, and the crate detects the mismatch instead
   of misattributing the reply. Not a memory-safety bug.
4. Upstream: read-only web search for tokio-modbus cancellation / transaction
   id mismatch / reuse issues found no matching issue or PR. Status: **new to
   this exploration, upstream unknown**.
5. Patch contrast: not attempted (dependency-copy patch would need a
   cancel-aware drain or per-call id reset plus a fence before the next send;
   no minimal one-line fix). Recorded as not covered, not as fixed.
6. Normal/rejection paths: normal path is covered by preflight/B1 baseline.
   A separate probe (`logs/run.j6qLo7/probe-reject.log`) sent a read to an
   out-of-range register (server exception 2), then a normal read on the same
   handle, a write, drop + reconnect:
   `out_of_range=Err("modbus exception IllegalDataAddress") normal_after=Ok([100,0])
   write=Ok(()) after_reconnect=Ok([100,7])`. So a protocol exception does not
   desynchronise the connection; only an abandoned in-flight request does.
   A pre-survivor reconnect contrast for the cancellation shape was not run
   (the survivor must stay on the same handle); reconnect after the fact is the
   only documented-free recovery and is visible in the probe.

## What this does not show

- Only one direct handle, one register, function 6 target and function 3
  successor; no RTU, no pooling, no `set_slave`.
- Only two pending boundaries of one call site and two race modes; plans are
  bounded by `max_runs=64` (the search was exhaustive for the realised
  boundaries: 2 plans).
- `Settle { 0, 0 }`: the stale reply is observed by the next request, not by a
  late-settling task; no claim about quiet periods without a next request.
- No upstream maintainer contact and no claim that 0.17.0 is the only affected
  version.
