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
