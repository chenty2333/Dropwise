# BOLT: bolt-client 0.11.0, pooled and direct

## Selection (before the first cancellation run)

- Family: `bolt-client 0.11.0` with `bb8-bolt 0.8.0` / `bb8 0.8.6`.
- Reuse modes: pooled `Pool<Manager>::get()` (max size 1), and a direct
  `Client<Compat<BufStream<Stream>>>` held outside the target future.
- Operation: `RUN` + `PULL` of one auto-commit write on a unique node,
  `MERGE (k:DwBolt {id: $id}) SET k.v = $v RETURN k.v AS v`.
- Server: Neo4j 5.26.0 community, one disposable container, loopback only.
- Pending: `dropwise::net::Proxy` holds every server-to-client chunk for 50 ms.
- Config: `Flavor::MultiThread { workers: 1 }`, races
  `[Immediate, AfterWake]`, `max_cancellations = 1`, `max_runs = 64`,
  `scenario_timeout = 20 s`, `Settle { tokio_time: 0 }`.

Why: the family is on the Phase 4 R-P/R-H list, the public API is documented,
the Neo4j image was already local from the neo4rs study, and it is the one
family in this batch that can be used both pooled and direct with the same
client and server. The write gives the survivor a real effect and the
independent observer something to read.

## What was read before the ladder

Public `bolt-client 0.11.0` and `bb8-bolt 0.8.0` docs/examples, the Bolt
message descriptions (RUN/PULL/RESET), and Neo4j server docs. No client
cancellation/reset implementation and no upstream issue was read before the
first cancellation runs. The first implementation read happened only after the
pooled errors were observed, for triage.

## Shape

Each trial starts from a fresh proxy, pool/handle, observer connection and
unique node id. Setup warms the tested connection with one write. The target is
the same `ctx.target(env.write(1))`; the four levels differ only after the cut:

- B1: stop after the cancellation.
- B2: repeat the same write with the same value on the same pool/handle; only
  `Ok` is judged.
- B3: run a different query (a read of the same node) on the same pool/handle;
  only `Ok` is judged.
- M1: `Expect::Atomic`; the survivor writes 7, reads it back through the tested
  handle, reads a unique tag, and an independent `Client` (not through the
  proxy) reads the node before and after the survivor.

For the pooled path, the target future contains `pool.get()` as well as RUN
and PULL. With the proxy delay, `pool.get()` itself is Pending while
`bb8` runs the manager's `is_valid` reset, so the plans include a checkout
cut. The direct path has no checkout.

## Added explorations (after the original result)

Added only after seeing the original pooled error:

1. `M-checkout`: target only `pool.get()` and drop the connection. This
   isolates the first Pending point from RUN/PULL. It reproduces the same
   `request: Some(Reset)` error on both races.
2. `M-checkout-nocheck`: the same checkout target with
   `bb8::Builder::test_on_check_out(false)`. With no validation reset,
   `pool.get()` has no Pending point at all (0 plans); this confirms the cut
   is the validation reset, but it does not test reuse of a dirty query
   connection.
3. `M-reset`: direct handle, explicitly `client.reset()` immediately after
   the cut, then the original survivor. All trials pass, showing the direct
   failures are caller-recoverable through the public API.
4. `M-reset-delayed`: the same explicit reset after a 100 ms wait. It also
   passes, so a simple "wait for the reply" timing does not explain the pool
   result.
5. Instrumented `bb8-bolt` copy outside the repository: prints
   `server_state` and the result of each `is_valid` reset. It shows a reset
   starting from `Ready`, sometimes from `Interrupted`, and one reset call
   whose future was dropped before completion (no trailing reset line) when the
   checkout target is cancelled.

## Triage reference

Upstream issue 0xSiO/bolt-rs#8 (open, 2021) describes exactly the underlying
limitation: dropping an unexhausted result stream leaves messages unread, and
RESET reads the next message rather than a correlated response. `bolt-client`
0.11.0 still has this shape. The pooled result is therefore a dynamic trigger
of a known upstream limitation through a cancelled pool checkout, not a new
bug. The direct result is the same limitation at the caller boundary; explicit
RESET works in the observed cases, but issue #8 warns it is not generally
reliable when messages are queued.

## Files

- `tests/bolt.rs`: ladder plus added explorations.
- `tests/probe.rs`: initial API check (kept for reproducibility, not part of
  the ladder). The earlier `tests/pooled_probe.rs` scratch probe had a
  `bolt://` prefix bug and was removed.
- `run.sh`: disposable Neo4j container and three repetitions.
- `logs/`: raw output.
- `environment.txt`: versions and image digest.

## Round 2: validation-cancellation patch (2026-10-21)

Round 2 asked whether the round-1 pooled failure is caused by the
validation reset being cancelled during pool.get(), and whether marking a
connection invalid until is_valid completes removes it. The original ladder
was not changed. Two separate projects were added so the registry baseline
stays reproducible:

- round2/original: unchanged bb8 0.8.6 from the registry.
- round2/fixed: a vendored copy of bb8 0.8.6 with the t2-bb8
  invalid-until-validated idea applied to Pool::get. The connection is marked
  Invalid before awaiting is_valid and restored to Present only on success.

The two projects share round2/shared/round2.rs. Both use the same Neo4j image,
proxy delay (50 ms), Config, races, target query, observer, and unique-node
setup as round 1. Tests are selected with --exact; no substring filter is used.
Each level runs three repetitions: normal_checkout, is_valid_failure,
checkout_only (pool.get() and drop, no RUN/PULL), pooled_b2, pooled_b3, and
pooled_m. The checkout-only target isolates the checkout cut from RUN/PULL.
The normal and is_valid-failure checks show the patch does not break the
ordinary checkout path or bb8's is_valid-Err/discard path. The latter uses an
injected manager error, not a real server validation failure.

Why two projects rather than one manifest: a path patch replaces bb8 for every
dependency in the build, so a single manifest cannot run the registry 0.8.6
baseline and the patched copy in the same lockfile. The first-round Cargo.toml,
Cargo.lock, tests/, logs/, and environment.txt were left untouched. The
round-2 lockfiles differ from round 1 only by the root package name, the
already-present async-trait direct dependency, and (for the fixed lock) the bb8
source/checksum becoming a path; no crate version changed.

The 0.9.1 patch applies semantically unchanged to 0.8.6. The only functional
difference inside Pool::get is that 0.9.1 calls record_get_started() before
the timeout; the hunk offset is different, so the change was applied manually.
A cancelled validation on the fixed copy discards the connection before it can
be returned to idle; the original returns the half-validated connection and
the survivor's next request reads a stale Reset response. The fixed copy still
created a new proxy connection and a second pool connection, so the result is
not "the cut became unrealized".

The replay test explores the original checkout scenario, takes the first
actual violation, calls Trial::replay_plan, and attempts replay_observed with
it. The source and replay call site is ctx.target(env.checkout()) at
round2/shared/round2.rs:333:31.

Round-2 numbers, the per-rep table, the old-vs-fixed pool/proxy statistics,
the replay result, and the direct-responsibility evidence (protocol state vs
library promise vs caller obligation) are in RESULTS-FRAGMENT.md. Raw output:
logs/round2-*.log. Exact commands are in run-round2.sh and in each log header.
No upstream issue, PR, or repository was modified.
