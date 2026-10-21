# tokio-modbus 0.17.0 — not started (round 3)

Status: **not started**; no ladder, no artifacts beyond this note. This is a
record of a timebox stop, not a result and not evidence about the client.

- The lead's mid-run timebox asked for a report within ~20–25 minutes and to
  prioritise skytable depth over adding modbus/pop. Skytable (two reuse modes,
  preflight, 3 reps per level, recovery, generated cases) consumed that budget.
- Environment was not blocked: the plan was a standalone Python asyncio Modbus
  TCP server (public framing, functions 3/6/16, configurable reply delay,
  loopback ephemeral port, server-side state/log endpoint) as an independent
  process, with a direct `tokio_modbus::client::Context`. No container is
  required. None of this was written or run.
- Open risk noted for a future attempt: tokio-modbus 0.17.0 has no per-request
  transaction-id argument in the public `Context` API; the server model must
  accept the client's ids and log them, and the survivor/observer must record
  whether a cancelled reply is consumed by the next request.
