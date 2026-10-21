# skytable 0.8.12 — cancellation reuse scenario (round 3)

## Family and versions

- Client: `skytable = 0.8.12` (official Rust driver, Skyhash/2 protocol).
- Pool: `skytable::pool::get_async(1, Config)` — the official async pool, which
  is a `bb8 0.9.1` pool (`ConnectionMgrTcp`). Pool size is 1.
- Direct: one `skytable::ConnectionAsync` (from `Config::connect_async`) held
  outside the target future.
- Server: `skytable/skytable:v0.8.4`, loopback-only ephemeral port in a
  disposable container named `dw-r3-sky-$$`.
- Tokio: 1.54.0, `Flavor::MultiThread { workers: 1 }`.

Skytable is a BlueQL/NoSQL database. It has **no transactions and no separate
authentication round trip**: `Config` carries the root credentials and the
driver authenticates during connection setup. There is therefore no
state-establishing operation other than the connection/checkout handshake and
an ordinary DML query. This scenario does not invent a transaction. The state
that outlives a cancelled target is: (a) the server-side row value, and (b) the
tested connection's unread reply bytes / the pool slot's protocol position.

## Operations

Setup (independent observer, direct connection, never through the proxy):

1. `create space <unique>`; `create model <space>.kv(id: string, v: uint64)`;
   insert row `id="k", v=0` (generated cases also seed `id="alt", v=0`).
2. Start `dropwise::net::Proxy` in front of the server with
   `ProxyOptions { reply_delay: 50 ms }`.
3. Pooled: `pool::get_async(1, proxy-config)` and one warm-up
   `pool.get()` + `sysctl report status`, then drop the pooled handle.
   Direct: `Config::connect_async()` to the proxy address.

Target (both modes): one BlueQL `update <space>.kv set v = 1 where id = "k"`.

- Pooled: the future is `pool.get()` **plus** the update, so the checkout and
  its validation query are inside the target as well. This is the reuse unit
  ("next borrower") and matches the BOLT round-2 lesson that pool checkout is a
  cancellation boundary. It is stated here, not discovered after the fact.
- Direct: the long-lived `ConnectionAsync` is held by the scenario and the
  target only borrows it; the handle is not moved into the dropped future.

B2 retries exactly the same update (`v = 1`, same row) through the same
handle/pool. B3 performs a different operation of the same kind: `select v from
<space>.kv where id = "k"` through the same handle/pool. B2/B3 judge only "not
an error"; their concrete values are printed for the method comparison.

M1 survivor: through the same handle/pool, update the **same row** to `v = 7`,
then `select` the same row back. The independent observer (a separate direct
`ConnectionAsync` opened in setup, bypassing the proxy and the pool) reads the
same row before and after the survivor.

## Pending

One mechanism for every level: `dropwise::net::Proxy` with
`reply_delay = 50 ms`, applied to each server→client chunk of the tested
connection. This is real time; the flavor is multi-thread. No server-side delay
primitive exists for Skytable queries. The observer does not pass through the
proxy.

## Observation (M1)

```text
before  = observer select v        (0 = target not applied, 1 = applied)
write   = survivor update v=7      (Ok / Err(text))
own     = same-handle select v     (expected 7)
visible = observer select v        (expected 7)
```

Pre calibration skips the target; post calibation completes it. The two
observations differ in `before` (0 vs 1) and are stable across two samples, so
the observer distinguishes "target never ran" from "target completed". A
cancelled target that the server applied but the client did not read yields
`before = 1` and is therefore compared with post, not treated as a violation by
itself. `Expect::Atomic`: the survivor must see a state equal to one of those
two calibrations.

## Contract read first

- Public docs (README, docs.rs API signatures) describe `ConnectionAsync::query`
  as one request/response and `pool::get_async` as a bb8 pool; there is no
  statement about dropping a query future, reusing the connection/pool slot, or
  transaction/rollback semantics (Skytable has no transactions). No cancellation
  guarantee and no documented recovery action for a cancelled query.
- Therefore "reuse after cancellation" is judged from concrete server state and
  the same-handle read-back, not from `Ok`. `Response::Error(u16)` is *not* an
  `Err` in this client; every helper converts it to an error string, because
  `query()` returns `Ok(Response::Error(code))` for server-side errors.

## Recovery contrast

No skytable public API documents a "cancel → reset/recover" action. The only
practical contrast is discard + reconnect: after a cancelled target, open a
fresh direct `ConnectionAsync` and read/update the same row. This is printed
separately (`SKY-RECOVERY`) and is **not** part of the survivor/level path; it
shows which layer is damaged (connection-local reply association vs server row).

## Levels and files

- `src/lib.rs`: env/setup/query helpers, observation, calibration.
- `src/bin/sky-ladder.rs`: preflight, B1/B2/B3/M1, recovery.
- `src/bin/sky-generate.rs`: `prospective/support/combine.rs` target × successor
  cases (pooled): update target × {update, update+read, select} × same/different
  object × same/different payload where both sides declare the domain.
- `run-ladder.sh`: disposable server, loopback ports, fresh `logs/run.XXXXXX/`.
