# Round-3 prospective predictions (written before the batch results)

Time: 2026-10-21, after reading the plan/targets/theory and the round-1/2
results, **before** any round-3 client or Linux result was returned. This is a
working prediction record for the lead's own calibration, not a preregistered
study and not Phase 4 data.

The question it is meant to answer: does the current commonality ("an
unfinished exchange leaves state behind; the party that decides reuse does not
receive, check, or correctly interpret that state") tell us **where to look**
before we know the answer, and where does it fail to predict?

## Predictions for new Rust clients

| family / mode | predicted outcome | mechanism expected | would falsify |
|---|---|---|---|
| skytable direct | likely violation | cancel a query after the server processed it; next request reads the previous reply (in-order protocol, no request id) | client discards/reconnects on drop, or server replies are framed so a stale reply is rejected |
| skytable pool | likely clean or same as direct | official pool may hand back the same connection without validation | pool validates/discards on error |
| tokio-modbus direct | uncertain, lean violation | MBAP transaction id lets the client detect a stale reply; if it ignores the id, survivor gets the previous register value | client checks transaction id and drains/discards mismatches |
| async-pop direct | likely violation | multi-line RETR body interrupted; next command reads leftover lines as its response | client buffers/resets POP3 state on drop, or server closes connection |
| orientdb-client pool | likely violation (BOLT-like) | mobc checkout/query cancellation leaves an unread reply; pool returns the connection | pool validates or drops the connection on cancellation |
| gel-tokio pool | likely violation | transaction-start round trip cancelled; server transaction left open, survivor inherits it | client sends rollback/reset on drop or pool discards |
| http-client H1Client | likely violation if it runs under Tokio | response body cancelled; connection returned to pool with unread bytes; next request gets leftovers | connection is closed/drained on cancellation |
| tiberius (not run this round) | expected violation if run | transaction BEGIN/rollback state on the server | documented caller-side recovery |

## Predictions for new Python packages

| package / mode | predicted outcome | mechanism expected | would falsify |
|---|---|---|---|
| psycopg pool+direct | likely confirmed | cancel around BEGIN/first write; connection left in transaction or aborted state; survivor commit/insert misbehaves | psycopg resets/rolls back on cancellation |
| asyncpg pool+direct | likely confirmed | transaction start cancelled; server transaction open; survivor inherits or errors | driver's connection state machine discards on cancel |
| aiomysql pool+direct | likely confirmed | MySQL has no cancel; client-side task cancellation leaves pending OK/ERR; next command reads stale packet | client closes connection on cancellation |
| mysql-connector-python direct | likely confirmed | same pending-packet association problem | connector reconnects |
| httpx pool | likely confirmed or clean | unread response body on the pooled connection | httpcore closes the connection on cancellation |
| curl-cffi pool | uncertain, lean clean | libcurl may own the transfer and close/reset it | native layer leaves partial transfer reusable |
| pymongo pool | likely confirmed/contract-external | transaction first round trip cancelled; server transaction open | driver documents and handles cancellation |
| neo4j pool | likely confirmed | BOLT-like reset/reuse race | driver resets on cancellation |
| aiosmtplib direct | likely confirmed | DATA 354/250 association, same shape as async-smtp | client rejects reuse after incomplete send |

## Predictions for Linux candidates

Ranked by my expectation of a contract violation (not by ease):

1. `mmap(MAP_FIXED)` replacement under allocation failure: old mapping is
   unmapped before `vm_area_alloc()`/`call_mmap()` can fail. Predict an error
   return with the old mapping gone. Contract may be weak (MAP_FIXED is
   destructive by design), so this may be a contract gap rather than a bug.
2. `mremap(MREMAP_FIXED)` / `MREMAP_DONTUNMAP`: destination unmapped before the
   move can fail; predict a hole at the destination on error.
3. `TCP_ULP("tls")` init failure: possible half-initialised ULP; uncertain.
4. `prctl(PR_SET_MM, ...)`: multi-field update may partially apply.
5. `io_uring` poll/timeout cancellation: possible cancel/completion race.
6. Everything else in the batch (setxattr replace, rename exchange, link,
   inotify update, timerfd, pipe resize, BPF filter replace, semtimedop,
   msgrcv, fallocate punch hole) is predicted **clean**, because the kernel
   allocates before committing or documents partial progress.

Falsification is useful: a clean result with the injection point proven and the
observer valid is evidence that the commonality is not universal; a violation
in a "predicted clean" target is a counterexample to my reading of the pattern.

## Predictions about method increment

- B1 should catch only hangs/panics; B2 should catch hard errors but miss stale
  values; B3 should catch some request-association errors but still miss values.
- M1 (concrete value + independent observer + pre/post) should be the only level
  that sees "Ok but wrong value" and "effect happened, return value wrong".
- I expect at least one case where B2 catches a problem that M1 does not (a
  transaction/conflict visible only on same-argument retry), and at least one
  case where B2/B3 pass while M1 fails.
- I expect the Linux fail-nth batch to be mostly clean; if the only findings are
  allocation-failure artifacts with no real trigger, they should be reported as
  such and not inflated.

## What would make me revise the commonality

- A new confirmed case where the signal exists and is correctly read, but the
  wrong decision is made for a reason unrelated to unfinished state.
- A new confirmed case where there is no unfinished exchange at all (pure
  success-without-effect, e.g. a success return that silently skips the effect).
- A family where simple `Ok`-checking finds as much as M1, repeated across
  several protocols: that would weaken the method-increment claim.
