# http-client 6.5.3 — preflight decision: not executed

**Decision: rejected for this round; no ladder, no container, not counted as an
executed target.** The rejection is about drivability under the existing Tokio
harness, not about the HTTP client's quality.

## Evidence

- `http-client` 6.5.3 features: `default = ["h1_client", "native-tls"]`,
  `h1_client = ["async-h1", "async-std", "dashmap", "deadpool", "futures"]`.
- Normal dependencies include `async-std ^1.6.0`, `async-h1 ^2.0.0`,
  `deadpool ^0.7.0`, `async-native-tls ^0.3.1`.
- `H1Client` is documented as "async-h1 based HTTP Client, with connection
  pooling". Its public surface is `new()`, `with_max_connections(usize)`
  (deprecated), `Config`, and `HttpClient::send(&self, Request) -> Pin<Box<dyn
  Future<...> + Send>>`.
- Connection establishment and the keep-alive pool are async-std tasks:
  `async_h1::connect`/`client` over async-std's `TcpStream`, with the pool
  driven by async-std's executor, not Tokio's I/O driver or task scheduler.

## Why that blocks the round-3 method

The existing ladder runs the whole scenario inside Dropwise's Tokio runtime
(`Flavor::MultiThread`) and cuts the future with the Tokio scheduler. An
async-std-based client needs either:

1. an async-std runtime running the client (a second, parallel runtime and a
   bridge boundary), or
2. a Tokio↔async-std I/O compat shim plus a spawned async-std executor for the
   pool internals.

Either introduces a **new runtime workflow** (thread(s), bridge, shutdown and
cancellation semantics at the boundary). Round 3's rule is to execute a target
only when it can be driven inside the same scenario without such a workflow.
`H1Client::send`'s boxed future does not remove the executor requirement: the
underlying `async_h1` connection and the `deadpool` objects are async-std
objects, not Tokio tasks.

`hyper_client` (feature `hyper_client`) is the Tokio-compatible alternative, but
that is a different client/feature than the required `H1Client`, and swapping it
would not test the requested target. It was not executed.

## What would be needed to revisit

- A documented async-std→Tokio bridge that preserves cancellation semantics at
  the boundary and is accepted as part of the scenario, **or**
- a target that is known to be driven solely by Tokio (hyper_client), treated as
  a separate family, not as H1Client.

No source inspection of the cancellation path was attempted (preflight stopped
at the runtime boundary). No container was started; no logs or counts exist for
this family.
