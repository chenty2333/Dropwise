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
