# tokio-modbus 0.17.0 — direct `client::Context` cancellation reuse (round 3)

## Family and versions

- Client: `tokio-modbus = "=0.17.0"`, `default-features = false, features = ["tcp"]`,
  direct `tokio_modbus::client::Context` from `client::tcp::connect`. No pool,
  no manager.
- Server: standalone `python3` asyncio Modbus TCP server
  (`modbus_server.py`, public MBAP framing, functions 3/6/16, four holding
  registers, JSONL request log). Loopback ephemeral port; no container.
- Pending: `dropwise::net::Proxy` with `ProxyOptions { reply_delay: 50 ms }`
  on the tested connection only. The Python server's own `--delay-ms` is 0, so
  there is exactly one pending mechanism.
- Observer: a second `client::Context` connected **straight to the Python
  server** (never through the proxy); the server request log is a second
  independent record.
- Tokio 1.54.0, `Flavor::MultiThread { workers: 1 }`.

## Operations and reuse unit

The direct handle is created once per scenario run in setup and held **outside**
every target future; the survivor and B2/B3 reuse that same `Context`. No
reconnect is performed before the survivor. `Proxy::connections()` stays at 1
for the whole run (setup includes one warm-up read).

Registers seeded to `[100, 0, 0, 0]` by the observer connection (function 16)
at the start of each run.

- Target: `write_single_register(1, 1)` (function 6). Single public call, but
  internally it writes/flushes the ADU and awaits the reply (several awaits);
  cancellation can land before or after the request leaves the socket.
- B2: `write_single_register(1, 1)` again, same handle, same arguments.
- B3: `read_holding_registers(0, 2)` (function 3), same handle.
- M1 survivor: `write_single_register(1, 7)` then `read_holding_registers(0, 2)`
  on the same handle.
- Observer (before/visible): `read_holding_registers(0, 2)` on the independent
  Context, giving `[100, x]`.

`write_single_register` is not a state-establishing operation in Modbus (there
is no session/transaction to establish); the state that outlives the cancelled
target is the register value and the tested connection's unread reply bytes /
transaction-id position. The preflight establishes that the handle works; it
does not invent a second pending operation.

## Observation (M1)

```text
before  = observer read regs 0..2   ([100, 0] = target not applied, [100, 1] = applied)
write   = survivor write reg1=7     (Ok / Err(text))
own     = same-handle read regs     ([100, 7] expected)
visible = observer read regs        ([100, 7] if the survivor write reached the server)
```

`Expect::Atomic`: the cancelled target may be "not applied" (`pre`, before
`[100,0]`) or "completed" (`post`, before `[100,1]`); the survivor must leave a
state equal to one of them. Pre samples are stable `[100,0]`, post samples
stable `[100,1]`, so the observer distinguishes them. A target applied
server-side but dropped client-side is `Post`, not a violation by itself; the
M1 violation comes from the survivor's own concrete results.

## Contract read first

Public docs of `client::Context` describe request/response methods and
`disconnect`, and the 0.17 implementation verifies MBAP transaction ids and
function codes on every response (`service/tcp.rs::call`,
`verify_response_header`). Nothing in the public docs promises that dropping a
request future leaves the connection usable, and there is no public
cancel/reset/drain API. Therefore cancellation reuse is judged from the
concrete next-request result and the independent register state; a documented
"not guaranteed" would make the case contract-external.

## Pending and files

- `src/main.rs`: setup, preflight, manual calibration, B1/B2/B3/M1.
- `modbus_server.py`: standalone server + request log.
- `run-ladder.sh`: fresh server on an ephemeral loopback port, fresh
  `logs/run.XXXXXX/`, cleanup trap.
- Raw logs: `logs/run.j6qLo7/` (preflight, 2 pre + 2 post calibrations,
  3 ladder repetitions, `server-requests.jsonl`).

## Recovery contrast (not part of the ladder)

No reset/recovery API exists on `Context`. The only recovery is drop + new TCP
connection; that is recorded as a coverage note, not exercised as a survivor.
