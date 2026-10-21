# Upstream PostgreSQL cancellation baseline

A small, reproducible probe of a mature upstream stack behind a byte-level
proxy, used as the strong baseline for the online-guards experiments.  It is
**not** a Dropwise result and it does not modify the server.

Stack: `tokio-postgres 0.7.18` + `bb8 0.9.1` + `bb8-postgres 0.9.0` (Tokio
1.54.0), pinned in `Cargo.toml` and `Cargo.lock`.  The proxy holds the first
bytes of a selected server reply and either releases them or keeps the TCP
connection open; the driver cancels the client future while it is frozen.

## Run

```sh
docker run -d --rm --name dw-upstream-repro \
  -e POSTGRES_USER=dropwise -e POSTGRES_PASSWORD=dropwise -e POSTGRES_DB=dropwise \
  -p 127.0.0.1::5432 postgres:18-alpine
PORT=$(docker port dw-upstream-repro 5432/tcp | awk -F: '{print $2}')

for mode in typed-begin blackhole commit validate; do
  DWPORT=$PORT cargo run --offline -- "$mode"
done
docker stop dw-upstream-repro
```

Environment: `DWHOST` (127.0.0.1), `DWPORT` (required), `DWUSER`/`DWPASS`/`DWDB`
(default `dropwise`).  The proxy binds `127.0.0.1:0`; the direct observer
connects to PostgreSQL outside the proxy so it is not affected by the cut.

## Modes

| mode | fault | question |
|---|---|---|
| `typed-begin` | hold the BEGIN reply, cancel `client.transaction()`, release | does the typed API clean up, and is the same connection usable? |
| `blackhole` | hold the BEGIN reply forever, return the pooled connection, call `pool.get()` twice | does bb8 detect a half-open connection or hand it out again? |
| `commit` | hold the COMMIT reply, cancel `tx.commit()`, check the row independently | did the cancelled commit commit? |
| `validate` | open a transaction, return the connection, let bb8 check it out | does `is_valid` prove a clean transaction? |

## Measured output (postgres:18-alpine, 2026-10-22)

```
typed-begin: reply frozen; cancelling the BEGIN future
typed-begin: is_closed while held = false
observer: pid=103 state=Some("idle") in_xact=Some(false) query=Some("ROLLBACK")
typed-begin: successor after release OK (3 messages)
observer: pid=103 state=Some("idle") in_xact=Some(false) query=Some("SELECT 1")

blackhole: checked-out connection returned to the pool
observer: pid=106 state=Some("idle") in_xact=Some(false) query=Some("ROLLBACK")
blackhole: successor get #1 after 701.801145ms: TimedOut
blackhole: state connections=1 idle=1
blackhole: successor get #2 after 701.312911ms: TimedOut
blackhole: state connections=1 idle=1
blackhole: proxy accepted 1 connection(s)

commit: reply frozen; cancelling the COMMIT future
commit: independent observer sees committed rows = 1
commit: successor after release OK (3 messages)

validate: opened a transaction on pid=113, returning the connection
observer: pid=113 state=Some("idle in transaction") in_xact=Some(true) query=Some("BEGIN")
validate: get succeeded after 511.748µs on an open transaction
validate: query inside the inherited transaction returned 1
```

## What the baseline establishes

1. **Typed BEGIN is already strong at the application boundary.**  The
   `RollbackIfNotDone` guard sends `ROLLBACK`; the server is `idle` before the
   held BEGIN reply is released, and the same backend is usable afterwards.
   The online guard must not claim this as a new capability.
2. **A half-open connection is not detected.**  `Client::is_closed()` stays
   `false`; there is no per-request timeout.  Unpatched bb8 times out the
   checkout and returns the same wedged connection to idle, so two successive
   `get()` calls time out instead of replacing it.  This is the pool-level
   negative result the guard's quarantine/recovery path targets.
3. **Validation is status-blind.**  `bb8-postgres::is_valid` is an empty simple
   query; it accepts `idle in transaction` and even an aborted transaction.  A
   successful checkout is not evidence of a clean transaction.
4. **A cut COMMIT reply is commit-in-doubt.**  The server committed
   (`committed rows = 1`) while the client only saw cancellation.  No client or
   pool can guarantee rollback here; the effect must be measured externally.

The guard experiment in `../online-guards` uses the same fault shape
(`CutPoint` at the write/read boundary) and reports a witness, quarantine and
retirement instead of treating "the next query succeeded" as proof.  See
`research/online/RESULTS.md` for the comparison and the remaining limits.

## Caveats

- The pool's `connection_timeout` is 700 ms and `max_size` is 1 to force reuse.
  A larger pool can hide the wedged object by serving the successor from a new
  connection; that is availability, not recovery.
- `bb8`'s timeout path is version-specific.  The observation is for the pinned
  `bb8 0.9.1` / `bb8-postgres 0.9.0`; do not generalise it to every pool.
- `tokio-postgres 0.7.8`'s builder path and raw `batch_execute("BEGIN")` have no
  rollback guard and do leak the transaction.  They are not the baseline API
  used here, but they are real version/API sensitivities.
- The probe measures one process on loopback with a cooperative proxy.  A real
  middlebox may send RST sooner; that changes the client outcome to "closed",
  which is detected, not to "clean".
