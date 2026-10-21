# Gel 7.1 cancellation scenario — probe only (round 3)

Status: **connectivity + one short cancellation probe, not a ladder.** The
timebox ended before the four-level method could be run for this family; this
file records the scenario that would be used and how far it got. Do not read the
probe as a B1/B2/B3/M1 result.

## Versions / service

| item | value |
|---|---|
| client | gel-tokio **0.11.0**, resolved `Config::max_concurrency = Some(1)` |
| runtime | Tokio 1.54, single-thread test runtime for the probe |
| server | `geldata/gel:7` → `geldata/gel@sha256:b7270b0973da6950d01ae0d578c6d38cd8d87fabdd6c4b75a09b74291ad6f3a8` (server 7.1+08db576) |
| bootstrap | `GEL_SERVER_SECURITY=insecure_dev_mode` (Trust auth, generated self-signed TLS; client `TlsSecurity::Insecure`) |
| ports | 5656 mapped to `127.0.0.1:32840` |
| build note | `gel-stream 0.4.5` + `derive_more 2.1.1` fails on rustc 1.98 (`#[try_from(repr)]` attribute expected); `cargo update -p derive_more --precise 2.0.1` in `Cargo.lock` is required. No client source was patched. |

Container `dw-r3-gel-424033` was started for this probe; it was not removed at
the time of writing (stop it explicitly).

## Intended ladder (not run)

- Setup: independent observer `Client` direct to server; create type `DwGel`
  (`k: str`, `v: int64`, `marker: str`); insert `{v: 0, marker: "seed"}`;
  tested `Client` through `dropwise::net::Proxy` with `reply_delay=50 ms` and
  `max_concurrency(1)`; `ensure_connected()` in setup so the target is not
  connection establishment.
- Target: first transaction round trip —
  `client.transaction(|tx| tx.query("with u := (update DwGel ... ) select
  (u.v, u.marker)", ...))` — with the same script used by survivor reads.
- Survivor: same `Client` (same pool) + same record, `v = 7`,
  `marker = "survivor"`; independent observer `Client` reads `(v, marker)`.
- Levels B1/B2/B3/M1 with the standard round-3 config; recovery contrast would
  use the documented transaction abort/rollback path.
- Generated cases via `combine.rs`: targets `query`/`transaction`, successors
  `update`, `update+read`, `read`, same/different object and payload.

## Probe actually run (`tests/gel_probe.rs`, 3 reps)

1. observer direct client; create type; insert unique record `v=0, seed`;
2. tested `Client` via proxy, `max_concurrency(1)`, `ensure_connected`;
3. target = `client.transaction(update v=1, marker='target' + select)`, cut by
   `tokio::time::timeout(10 ms)`;
4. after cancel: observer read; then survivor transaction on the **same
   client** (`v=7, survivor`); same-client read and observer read; print proxy
   connections (cumulative).

## Not covered

No B1/B2/B3/M1 counts, no calibration table, no generated cases, no recovery
contrast, no reuse diagnostic beyond `max_concurrency=1`, no observer-visible
pre/post baseline, no 3-repetition ladder, no transaction rollback assertion,
no source inspection. Coverage is one cancellation point × 3 reps.

---

**Status update (append-only, 2026-10-21):** the intended ladder has now been
run; see `RESULTS-FRAGMENT-LADDER.md` and `logs/run.Zlfofp/`. The probe section
above is retained as historical context. The tested mode is the single
connection pool (`max_concurrency(1)`); no true direct handle exists in
gel-tokio 0.11.0.
