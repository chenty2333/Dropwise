# Phase 3 amendments

Only additions are allowed here; `PLAN.md`, `targets.md` and `FREEZE.txt` are
frozen. Each entry states the date, the reason, and whether it was decided
before or after any result for that target was seen.

## Database targets (`db-clients/`, 2026-10-19)

### 1. clickhouse-arrow features: `inner_pool` disabled

The crate's default features include `inner_pool`, which gives one `Client`
four native connections internally. The frozen target is “clickhouse-arrow
(bb8 manager, 原生 TCP 协议), 单条原生协议连接”, so the experiment builds it
with `default-features = false, features = ["derive", "serde", "pool"]`: one
`Client` is one native TCP connection and the bb8 manager is the only pooling
layer. Decided **before** any clickhouse result, while writing the manifest.

### 2. MongoDB is a single-node replica set

`targets.md` only says `mongo` image. The session-state operation chosen for
the first target is a transaction, and a transaction needs a replica set, so
the container runs `mongo:7.0.21 --replSet rs0 --bind_ip_all` (published on
127.0.0.1 only) with `rs.initiate({_id:"rs0",members:[{host:"127.0.0.1:27017"}]})`,
and clients use `directConnection=true` so the advertised in-container address
does not matter. Decided **before** any mongodb result, while setting up the
server.

### 3. One Pending mechanism per crate: `Proxy::reply_delay`

The four levels (and M1-residue) of a crate use the same mechanism:
`dropwise::net::Proxy` with `ProxyOptions { reply_delay: 50 ms }` holds every
server-to-client chunk. This is needed for `START TRANSACTION` (mysql_async)
and `USE`/the in-transaction first command (clickhouse-arrow, mongodb), which
have no server-side `SLEEP`. It is applied to the ordinary-request operation as
well so all levels inside one crate share one mechanism. Decided **before**
the runs; PLAN.md §3 explicitly allows the proxy's held replies.

### 4. mysql_async “patched” variant is a crate patch plus a caller guard

The release run showed that a directly held `Conn` whose query future was
cancelled returns the abandoned query's result to the next query. The library
patch (`patches/mysql_async/mysql_async-0.37.1-connection-closed.diff`) makes
that next command fail with `Connection to the server is closed` instead of
silently reading the stale reply. The `patched` run also sets
`DW_MYSQL_GUARD=1`, which is what a correct caller must do: check
`Conn::is_disconnected()` and replace the connection. The patch was written
**after** the release result was seen; it is a triage contrast, not part of the
detection method. The `release` numbers are the ones counted in Q1.

### 5. M1-residue waits 100 ms before sampling

`Proxy::residue()` only sees bytes that have reached the client socket. With a
50 ms held reply, the residue variant sleeps 100 ms of real time after the
cancellation, before reading the residue and before the survivor starts. This
is the same technique as the earlier residue experiments and does not change
the Pending mechanism. Decided **before** the residue runs.

### 6. Image tags

`mongo:7.0.21` is pinned in `run-db.sh`; MariaDB remains the local
`mariadb:11.4`. Decided **before** the runs, per the task's fixed-tag
requirement.

### 7. ClickHouse image: 25.5.2.47 instead of 24.8.14.39

The first pinned ClickHouse tag, `clickhouse/clickhouse-server:24.8.14.39-alpine`
(server revision 54469), cannot speak to clickhouse-arrow 0.2.1 at all: the
client's pre-54470 reader branch forces `ChunkedOptional`, which
`supports_chunked_*` treats as chunked, while the server does not know chunked
packets (`Unknown packet 52 from client`); every DDL times out. The ladder was
therefore run against the crate's own tested server,
`clickhouse/clickhouse-server:25.5.2.47-alpine` (revision 54479). The 24.8
pairing is reported separately as an environment/compatibility finding, not as
a cancellation violation. Decided **after** the 24.8 handshake test, **before**
any cancellation result; the server change is a prerequisite for the frozen
target to be usable at all. A one-line patched-crate contrast on 24.8 confirms
the chunked-mode root cause.

## HTTP targets (`http-clients/`, 2026-10-19)

### 8. Results file: `RESULTS-http.md`

`PLAN.md` §4 says results go to `RESULTS.md`. The coordination for this session
asked the HTTP half to write `RESULTS-http.md` and leave `RESULTS.md` to the
database half, which merges both. Decided **before** the runs; not a
methodological change.

### 9. Two request shapes (L1/L2) for the one chosen operation

`PLAN.md` §2 selects one ordinary request per crate (no session-establishing
call exists in these five clients). That one operation is a complete request
including reading the whole response body, and it was exercised in two shapes:
L1 is a small GET; L2 is a POST with a 16 MiB body, with the server delaying
its read of request bodies larger than 1 MiB. L2 was added **after** the L1
release matrix came back all zero and **before** any L2 result was seen: the
GET shape cannot return `Pending` while the request body is being sent, and
that is the other place a single HTTP/1.1 byte stream can be left mid-message.
L1 and L2 are two inputs of the same target operation, not a second operation
in the sense of §2; the four levels and the Pending mechanism are identical
inside each shape. (For hyper-util and reqwest the caller future does not
expose write-side `Pending` boundaries anyway because an internal connection
task writes the body; for awc and pingora it does.)

### 10. Pending mechanism and response padding

Within a shape, all four levels (and M1-residue) use the same mechanism: the in-process
HTTP/1.1 server sleeps 40 ms before the response header and 80 ms more before
the body; every reply body is `repl:<id>` plus 256 KiB of padding so that body
reads have `Pending` boundaries of their own; L2 additionally sleeps 150 ms
before reading a request body over 1 MiB; it answers `Expect: 100-continue`
before that wait so isahc does not fall back to its 1 s client-side timeout.
These are the "slow in-process fake server" and "large load" mechanisms allowed
by `PLAN.md` §3. The padding was
chosen before the measured runs so that isahc's body reader cannot buffer the
whole reply in one read. Decided **before** the runs.

### 11. M1-residue uses `Proxy` only for the residue probe

M-residue sends the client through `dropwise::net::Proxy` and samples
`Proxy::residue()` after a fixed 200 ms real-time wait, then adds the largest
unread count per client socket to the observation. The Pending mechanism is
still the server delays, not `ProxyOptions::reply_delay`; the proxy is present
only to observe residue. `PLAN.md` §3 allows the proxy's held replies but does
not require them, and §3 explicitly separates M1-residue from the Q1 count.
Decided **before** the residue runs.

### 12. `reuse` diagnostic is not a ladder level

Each target binary also has a `reuse` test that sends three uncancelled
requests and prints the server's connection ids. It is not counted in the
ladder table; it only checks that the pool really reuses a connection before a
zero ladder result is interpreted. Decided **before** the measured runs.

### 13. No fixed/patched variant was run

No violation was confirmed in the release variant, so there is no patch or
configuration contrast to apply. `run-http.sh` implements the
`[patch.crates-io]` copy-and-patch path and skips the fixed variant with a
message when `http-clients/patches/MANIFEST` is empty. Decided **after** the
release results.

## Database targets, continued (`db-clients/`, 2026-10-19)

### 14. MongoDB harness: no per-run database drop; fresh server per repetition

The first mongodb runs dropped each run's database. A cancelled `MG1` leaves
an open server transaction; `dropDatabase` waits on its lock, the client's 3 s
timeout returns but the server-side drop keeps running and blocks later runs
(`observer setup insert timed out`, `cancelled=0`). Those were **test-harness
errors**, not cancellation evidence. The harness now leaves each run's unique
database in place and the runner starts a fresh `mongo:7.0.21` container per
repetition. Decided **after** the first poisoned runs, before the reported
3 × full ladders.

### 15. MongoDB patch contrast: one reported patched variant, one rejected attempt

`patches/mongodb/mongodb-3.9.1-cancelled-transaction-abort.diff` is the
patched variant used for the reported contrast. It has four parts: treat a
`Starting` transaction as `InProgress` in `ClientSession::drop`; register and
wait for the Drop-time abort; mark an abort `Aborted` only after the
`abortTransaction` command completes; and move the wait from
`ClientSession::new` to `ClientSession::start_session` (waiting from `new`
also blocked the abort task's own session creation and turned every MG1 M plan
into a liveness failure). The first three parts were tried first and fixed B2
but deadlocked M; that attempt is recorded in `logs/mongodb/` and the final
patch is the one the `patched` runner applies. Decided **after** seeing the
release B2 WriteConflicts and the first patch's M liveness failures; only the
patch contrast is affected, the release numbers counted in Q1 are unchanged.

## Triage (2026-10-19, reviewer)

### 16. MongoDB MG1 reclassified as contract-external

`RESULTS-db.md` counted MG1 (a transaction's first command cancelled, the
transaction left open on the server, the same-parameter retry failing with
`WriteConflict`) as a confirmed issue. The driver's README has a section
"Warning about timeouts / cancellation": dropping futures returned by the
driver "can leave the driver's internals in an inconsistent state" (RUST-937),
and users are told to poll all driver futures to completion, for example by
timing out on a spawned task's `JoinHandle` instead. `PLAN.md` §4 classifies
behaviour the library's documentation explicitly does not guarantee as
contract-external, which is not counted in Q1. The reproduction, the patch
contrast and the ladder numbers stay as recorded; only the classification
changes. Decided **after** the results, during review.

mysql_async MY2 stays a confirmed issue: neither its README nor its crate
documentation restricts using a `Conn` after a dropped call, and the
maintainer's description of cancellation (blackbeam/mysql_async#401) is that
the connection is left broken "from the pool's perspective", which a directly
held `Conn` does not check.
