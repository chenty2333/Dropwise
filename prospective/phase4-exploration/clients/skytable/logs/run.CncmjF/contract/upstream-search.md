# Read-only upstream search (2026-10-21)

No issue, PR, or comment was posted; no upstream contact.

## Client (`skytable/client-rust`)

- Enumerated all issues+PRs: `GET https://api.github.com/repos/skytable/client-rust/issues?state=all&per_page=100`
  -> 34 items (31 issues/PRs excluding later additions; list saved as
  `upstream-client-rust-issues.json`). Titles include #11 "Implement connection
  pooling", #13 "Entity/table for connection pooling?", #17 `&self` vs `&mut self`,
  #27 "Fix protocol decoder and response issues", #32 pool doc typo. None mention
  cancel/drop/timeout/reuse/desync/stale reply.
- GitHub issue search pages (read-only HTML) for `cancel`, `drop`, `desync`,
  `stale reply`, `pool reuse` in `skytable/client-rust`: 0 issue links each.
- Repository state: default branch `next` HEAD = `4fceb22` (2025-02-16),
  identical to tag `v0.8.12`; no later commit. `crates.io` newest version is
  still 0.8.12 (crate updated 2025-02-16). No post-0.8.12 fix in this repo.

## Server (`skytable/skytable`)

- GitHub issue search pages: `cancel` -> 1 link, #324 "Implement NodeJS client
  driver" (unrelated); `drop` -> 11 links, all DDL/drop/connection-termination/
  Windows-flush topics, none about reply association; `desync`, `stale reply`,
  `pool reuse` -> 0 links.
- The server has no client-cancellation primitive in the public protocol
  (query-time request/response only; no request id).

## Verdict

- **Known upstream? No matching report found.** This is "upstream unknown", not
  "new": absence of a report cannot establish novelty, and we did not contact
  the maintainers. No duplicate classification is claimed.
