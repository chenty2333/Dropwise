# Patch contrasts

These are not part of the release numbers. They exist to show a cause and a
fix for the M violations in `../../RESULTS.md`.

- `neo4rs/neo4rs-0.8.0-cancelled-exchange.diff`: add a `dirty` flag to
  `Connection`, set it before a `Graph::run`/`Graph::execute` exchange and clear
  it only after that exchange finished cleanly (`run_retryable` after RUN +
  DISCARD; `execute_retryable` when the `DetachedRowStream` reaches its terminal
  summary). `ConnectionManager::recycle` checks the flag before sending RESET
  and returns `Error::UnfinishedExchange`, so deadpool discards a connection
  whose exchange was cancelled instead of consuming a stale `SUCCESS` as the
  RESET acknowledgement.
- `neo4rs/Cargo.patch.toml`: the `[patch.crates-io]` fragment used by
  `../run-neo4rs.sh`; `@WORK@` is replaced with the work directory.

The patch is deliberately conservative (any unfinished exchange discards the
connection). Its purpose is the triage contrast, not a proposed upstream
implementation; upstream #298 uses a finer `pending_responses` / `is_reusable`
check. `run-neo4rs.sh` copies the cached registry source, applies the diff and
builds a copy of the scenario crate with the patched dependency.
