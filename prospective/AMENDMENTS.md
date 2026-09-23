# Amendments

## A1 (2026-09-23T15:37:24Z, before the first run against any target)

C2(b) says "tracked with Dropwise obligations on a test `ManageConnection`". While writing the
T2 scenarios it became clear that an obligation cannot tell a connection dropped by the pool
(legitimate) from one dropped with a cancelled `get()` future: both reach the same `Drop`.
C2(b) is therefore operationalised as: after the settle wait, with no connection checked
out, `pool.state().connections` equals the number of connection objects alive (counted by the
test manager). A pool that counts a connection that no longer exists has lost capacity; a
live connection the pool does not count has been lost by the pool. The contract text and all
other scenario definitions are unchanged.

## A2 (2026-09-23T16:18:30Z, before writing or running T3 scenarios)

T3 will be authored and tested by Codex (GPT), rather than the Claude session that tested T1
and T2. The preregistered server image was `postgres:16`; this host already has the
`postgres:18-alpine` image locally, so T3 will use that image instead. This is an explicit
server-version deviation for the secondary, exploratory target; no image pull is planned.

## A3 (2026-09-23T16:35:00Z, before the second scenario correction)

The first run failed in pool setup; correcting the runner to probe TCP (commit `d4aab47`)
did not resolve it. In sqlx 0.9.0, `PoolOptions::connect_with` bounds opening the pool with
`acquire_timeout` (30 seconds by default); with Dropwise's `Flavor::CurrentThread`, Tokio time
starts paused, so a real TCP connection can remain pending while the virtual timeout expires
before any target is registered. To exercise C3 while keeping every frozen `Config` field
unchanged, T3 will resume Tokio time only during pool creation/warm-up and post-race follow-up
queries, and pause it again around each `ctx.race` target and before scenario return/settling.
This is a T3 harness timing adjustment: external database I/O remains real-time, while the
registered cancellation delay and settle window use the frozen paused-clock behavior.

## A4 (2026-09-23T16:39:23Z, before the third scenario correction)

A3 let pool creation succeed, but S3.1-S3.3 still acquired from the pool inside the paused
`ctx.race` target and timed out before the query ran; S3.4 returned no Pending boundary because
the stream data was already buffered. The target will now acquire the pool's sole
`PoolConnection` during the real-time setup phase, race the `fetch_*` future on that pooled
connection, then return the lease before verifying eight follow-up queries through the same
pool. S3.4 will read three rows first, then stream 20,000 rows with a server-side 50 ms delay
after row 10,000, so the raced stream has a pending read after a prefix and before completion.
This is a scenario/harness correction for 4.3(a); it does not change C3 or any frozen Config
field.
