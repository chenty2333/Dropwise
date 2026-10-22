# psycopg: delayed BEGIN/SAVEPOINT, recovery and re-cancellation (group 2)

> Correction (2026-10-22): the "fail-closed entry fix" in this file is the
> older experimental rollback-in-`__aenter__` monkeypatch, not the submitted
> `_recover_entry_failure()` patch. The actual patch is verified separately by
> the probe and complete differential (clean entry, zero violations); its 5 s
> delay timings were not rerun. The timing table is retained as a strategy
> reference and is labelled accordingly.

Local PostgreSQL 18.4.  The tested handle goes through a loopback proxy that
delays only **server→client response chunks**; the request direction is not
delayed.  An independent observer polls the tested backend and the first cancel
is sent only after it sees the server in a transaction (`BEGIN` or the already
open outer transaction for a savepoint).  The 5 s delay is therefore an actual
in-flight reply, not an assumption about send order.

Three timings are kept separate:
(a) cancel request → caller receives cancellation;
(b) cancel request → recovery action completed / connection closed;
(c) cancel request → a fresh connection completes a visible successor write.
Old-resource release is watched separately after the successor measurement.

## Top-level entry, 5 s response delay

| recovery mode | (a) cancel→caller | recovery result | server after recovery | (b) cancel→old cleared | (c) cancel→successor visible |
|---|---:|---|---:|---:|
| none | 5.002 s | none | `INTRANS` | **not cleared in 7 s** | 5.011 s (new direct conn) |
| `conn.rollback()` | 5.002 s | **false ok**: `ProgrammingError: Explicit rollback() forbidden within a Transaction context` | `INTRANS` | **not cleared in 7 s** | 5.015 s |
| `conn.close()` | 5.001 s | close returned | cleared | **0.0006 s** | 5.011 s |
| experimental fail-closed entry fix | **10.004 s** | rollback inside cancellation | idle / `ROLLBACK` | 0.0005 s | 10.013 s |
| experimental fix + second cancel at 0.1 s | **0.102 s** | second cancel interrupts repair; fix closes | idle/closed | 0.0007 s | 0.112 s |
| reconnect + second cancel at 0.1 s | 0.101 s | recovery task cancelled; old cleared | closed | 0.0006 s | 0.215 s |

The dominant result is that **cancellation delivery itself is bounded by the
delayed BEGIN reply**: with a 5 s response delay, the caller does not observe
cancellation until 5 s, so an explicit recovery after the catch cannot start
earlier.  The experimental fail-closed fix does its rollback *inside* the cancelled
`__aenter__`; under a 5 s response delay it needs a second response round trip
and takes 10 s.  The second-cancel path is an escape: it returns in 0.1 s and
clears the old resource, but it is a second interruption, not a normal repair.

The `rollback()` result is deliberately classified as **false ok**: the
function returned a result object, but the result contains the
`ProgrammingError` and the server remains `INTRANS`.  This is not a successful
recovery.  The old resource is still held after 7 s.

## Short (50 ms) control

| mode | cancel→caller | recovery | cancel→successor |
|---|---:|---|---:|
| rollback | 0.157 s | false ok (guard) | 0.170 s |
| close | 0.156 s | closed | 0.165 s |
| reconnect | 0.154 s | reconnect 0.412 s | 0.576 s |
| none | 0.155 s | none | 0.163 s |

At 50 ms the delay is still visible in the cancellation path (~0.15 s rather
than the immediate sub-millisecond case), but the successor is available
quickly.

## Nested entry inside a legitimate outer transaction

Outer transaction is opened normally; the inner (`SAVEPOINT`) entry is
cancelled at 5 s response delay.

| mode | cancel→caller | inner/outer outcome | old resource |
|---|---:|---|---|
| original, then `rollback()` | 5.002 s | rollback guard fails; outer exit raises `OutOfOrderTransactionNesting`; outer remains open | not cleared in 7 s |
| experimental fail-closed fix | **15.005 s** | inner savepoint released; outer rollback succeeds; both writes invisible | cleared 0.0008 s |
| experimental fix, successor | 20.020 s | successor visible | — |

The original nested case is a stronger failure than top-level: the cancelled
inner entry leaves an active transaction object on the nesting stack, so even
the legitimate outer CM cannot be exited (`OutOfOrderTransactionNesting`).  A
successor direct connection is still usable, but the tested handle's outer
transaction is stuck.  The fix protects this boundary but costs 15 s to caller
and 20 s to successor under the 5 s response delay.

## Pool capacity

One-slot `psycopg_pool` (max_size=1), one-way 5 s response delay.  A timed-out
`getconn()` was deliberately **not** issued before the successor measurement,
because a timed-out waiter can remain queued and contaminate the next
observation.

| mode | cancel→caller | pool while held | successor | note |
|---|---:|---|---:|---|
| none | 5.002 s | size 1, available 0 | 20.012 s | reset/discard then new conn through delayed proxy |
| close | 5.001 s | size 1, available 0 | 20.011 s | old discarded; new connection |
| experimental fix | 10.002 s | size 1, available 0 | **0.000 s** | same connection retained after rollback |

The pool makes the cost explicit: the old lease occupies the only slot.  A
close/replace path must create and authenticate a new connection through the
delayed proxy, which took about 20 s (four 5 s response chunks).  The fix keeps
the connection and gives an immediate successor, at the price of a longer first
cancellation.

## Limits

- One host, one response delay (5 s), 1-2 reps per cell, experimental monkeypatch
  for the fix.  The proxy delays a fixed amount per response chunk; it does not
  model packet loss or reordering.
- The direct-close policy is measured for psycopg here; the MySQL half is in
  `mysql_delay/` (mysql-connector-python).  The two are not assumed equivalent.
- The successor is a fresh direct connection unless the row says "same
  connection"; it is a usability check, not a byte-identical path.
- No claim that 5 s is a worst case; a longer reply delay would extend (a) and
  the fix's recovery proportionally.
