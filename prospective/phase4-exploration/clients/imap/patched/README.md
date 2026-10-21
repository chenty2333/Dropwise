# Round-2 APPEND-continuation patch (triage only, not upstream)

This directory contains the round-2 validation of the APPEND-continuation
sketch that round 1 left in `logs/patched-async-imap.diff`. The code change is
the same loop; the round-2 patch adds comments and a regression suite. It is a
dependency-copy experiment, not a proposed upstream commit and not a general
cancellation framework.

## Change

- Source: `async-imap 0.12.0`, `src/client.rs`, published line 1177
  (`Session::append`).
- Patch: [append-continuation.round2.patch](append-continuation.round2.patch)
  (30 lines, sha256 `b4eea3f455f4d8773859aca8342c96d1d0bd2b92e43979d0b528130efc1dac0e`).
- Tests: [append-continuation-tests.round2.rs](append-continuation-tests.round2.rs)
  (194 lines, sha256 `9bc6b38d22a7bced71ef8f75bb3efd038281d2759939522f55b6db564f8c0f61`).

The patch replaces the single "next response must be `+`" read with a loop:

```rust
loop {
    let Some(res) = self.read_response().await? else {
        return Err(Error::Append);
    };
    match res.parsed() {
        Response::Continue { .. } => break,
        Response::Done { tag, .. } if tag == &id => return Err(Error::Append),
        _ => continue,
    }
}
```

Semantics:

- A `+` continuation ends the drain; the literal is then written and the
  normal tagged completion is checked by `check_done_ok_from`.
- Untagged responses left by a dropped command are skipped.
- A tagged completion for **another** command is ignored. This mirrors the
  tag-first pattern that async-imap PR #142 added to `login`
  ([PR #142](https://github.com/chatmail/async-imap/pull/142), saved as
  [../logs/round2-pr142.diff](../logs/round2-pr142.diff)): match on
  `Response::Done { tag, .. } if tag == id` before `check_status_ok`.
- A tagged completion for **this** APPEND before `+` is an error: the server
  cannot have accepted a literal it never received. Returning `Err(Append)`
  avoids swallowing that completion and avoids waiting forever.
- EOF before `+` is `Err(Append)`, not a hang.
- `NO`/`BAD` after `+` still reaches `check_done_ok_from` and returns
  `Err(No)`/`Err(Bad)`; `NO`/`BAD` before `+` returns promptly as
  `Err(Append)` on this code path (the generic error is a limitation; it is
  still an error, not a success or a hang).

It does **not** fix SELECT's `parse_mailbox` accumulation. A retry SELECT that
starts first still merges the abandoned SELECT's FLAGS/PERMANENTFLAGS, so
`Mailbox.flags`/`permanent_flags` can contain duplicates after a first-Pending
cut. It also cannot distinguish a stale `+` from the APPEND's own `+`; that
synthetic case times out in both the published and patched builds.

Two narrower limitations remain:

- `NO`/`BAD` **before** `+` is collapsed to `Err(Append)` by the patch. It errors
  promptly (the real-server tests take 51-52 ms), but the status detail is lost.
- **After** `+`, the subsequent tagged completion is still read by
  `check_done_ok_from`, which checks status before tag. An unrelated tagged
  `NO`/`BAD` arriving between `+` and this APPEND's own completion can therefore
  be reported as this APPEND's failure. The round-2 suite does not exercise that
  ordering; it is inferred from the 0.12.0 source, not observed on GreenMail.

## Files

- `apply-round2-patch.sh [--no-fix] DEST` — copies the cargo-registry 0.12.0
  source and injects the tests. Without `--no-fix` it applies the patch first.
  `DEST` must not exist.
- `run-round2-patch-tests.sh` — creates both copies under `/tmp`, runs the
  regression and full suites, `cargo fmt --check`, and clippy, and writes
  `logs/round2-patch-*`.
- The runner `../run-round2.sh` uses `apply-round2-patch.sh` to build the
  patched binary against a copied `round2/` crate with
  `[patch.crates-io] async-imap = { path = ... }`.

## Test evidence (2026-10-21)

Published 0.12.0 + injected tests:

- upstream tests (round-2 tests skipped): **73 passed, 0 failed**
- round-2 regression tests: **10 passed, 3 failed** (expected)
- failing, all with `Err(Append)`:
  `round2_append_skips_stale_select_responses`,
  `round2_append_skips_stale_no_for_other_command`,
  `round2_append_skips_stale_bad_for_other_command`

Patched copy:

- full suite: **86 passed, 0 failed** (73 upstream + 13 round-2)
- `cargo fmt --all -- --check`: **0**
- `cargo clippy --all-targets`: **0** (5 unique pre-existing warnings; the
  patched/unpatched warning sets are identical, diff empty)

Raw logs: [../logs/round2-patch-tests-summary.txt](../logs/round2-patch-tests-summary.txt),
[../logs/round2-patch-tests-unpatched.log](../logs/round2-patch-tests-unpatched.log),
[../logs/round2-patch-tests-patched.log](../logs/round2-patch-tests-patched.log).

## Real-server behavior

With GreenMail 2.1.14, 50 ms proxy, 5 s normal bound:

| case | published | patched |
|---|---|---|
| normal APPEND, selected | `Ok`, observer 1 | `Ok`, observer 1 |
| normal APPEND, unselected | `Ok`, observer 1 | `Ok`, observer 1 |
| nonexistent mailbox (NO after `+`) | `Err(No)`, 146 ms, observer 0 | `Err(No)`, 107 ms, observer 0 |
| invalid flags (BAD before `+`) | `Err(Append)`, 52 ms, observer 0 | `Err(Append)`, 52 ms, observer 0 |
| invalid date (BAD before `+`) | `Err(Append)`, 51 ms, observer 0 | `Err(Append)`, 52 ms, observer 0 |
| stale SELECT then APPEND | `Err(Append)`, 0 ms, observer 0 | **`Ok`, 237 ms, observer 1** |
| stale SELECT then rejected APPEND | `Err(Append)`, 0 ms, observer 0 | **`Err(No)`, 104 ms, observer 0** |
| fake EOF before `+` | `Err(Append)`, 40 ms | `Err(Append)`, 41 ms |
| fake own-tag NO before `+` | `Err(Append)`, 40 ms | `Err(Append)`, 40 ms |
| fake stale `+` | caller timeout, 2001 ms | caller timeout, 2001 ms |

The patched stale-SELECT transcript shows eight stale SELECT responses and the
stale tagged `A0002 OK` skipped, then `+ OK`, the literal, and
`A0003 OK [APPENDUID ...]`. The patched stale-reject transcript shows the same
drain, then `A0003 NO [TRYCREATE]`.


## Review update

The default apply/test runner now uses `append-continuation.review.patch`.
Unlike the round-2 sketch, it delivers unsolicited responses through the existing
channel and stops on BYE before sending the literal. Two review cases were added
to the test fragment: notifications before continuation and BYE before a literal.
Published code: 73 original tests pass; added cases have 11 passes / 4 expected
failures. Reviewed patch: 88 pass. Formatting passes; Clippy returns success with
the same five pre-existing warning categories (not a warning-free claim).
The sketch's stale-continuation limitation remains: a `+` is untagged, so this
change does not promise arbitrary cancellation recovery, particularly after
cancelling a previous APPEND while the server awaits its literal.

`APPEND_PATCH` may select the historical round-2 patch for comparison. Work
settings select a parent for a fresh temporary directory; no supplied directory
is recursively deleted. Logs go to a fresh run directory. No hashes or archive
are required to run these checks.
