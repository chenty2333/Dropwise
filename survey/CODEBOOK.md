# Annotation codebook (v3, 2026-09-23)

v3 revises v2.1 (archived as `CODEBOOK-v2.1.md`) after the first independent double annotation
(annotator A: Claude Opus 5.5, B: ChatGPT 6 Astra; 109 flagged + 60 sampled rows). Changes are
driven by the measured disagreements (Cohen's kappa A vs B in brackets):

| v2.1 field | kappa | v3 change |
|---|---|---|
| relevant | 0.78 | kept; scope rules for handles and never-awaited futures added |
| holder | 0.79 | kept; rule for values taken out of shared state |
| outlives, layer | 0.89, 0.97 | kept |
| cancel_source | 0.73 | `drop` renamed `owner_dropped`; combinators go to `select` |
| found_by | 0.73 | operational rule added |
| phase | 0.46 | fewer values, decided by an ordered procedure |
| after | 0.58 | values describe who uses the affected state next |
| violation | 0.44 (set) | one primary value + optional extras |
| tags | 0.19 | **removed** (not reliably codable) |
| injectable | 0.30 | **removed**; replaced by `reproduced`, filled only by actual reproduction work |

## Scope: what counts as a cancellation defect

A *cancellation* is the destruction of a future, or of the task owning it, after it has been
polled and before it completes: a losing `select!` branch or fail-fast combinator
(`try_join!`, `try_join_all`), an elapsed `timeout`, `abort()`, the owner dropping it (a server
dropping a handler when the client disconnects, a collection being cleared), runtime shutdown.

Out of scope:
- a future dropped **before its first poll** because of a missing `.await` (`let _ = fut;`):
  `no` (a different, lint-covered class);
- a non-future **handle** dropped mid-operation (an h2 `SendStream`, a tonic `Channel`, a
  service object leaving a cache) without a future or task being destroyed: `adjacent`;
- cooperative cancellation where the operation observes a token/flag and returns normally, and
  futurelock (a future kept but no longer polled): `adjacent`.

## relevant
- `yes`: a real in-scope defect with evidence (rule E below).
- `adjacent`: a related defect that is out of scope for the reasons above.
- `dup`: same defect as another row of your sheets; `fix_url` = the primary row's URL.
- `unclear`: in scope if true, but evidence is insufficient or the cause cannot be established.
- `no`: docs, questions, features, refactors, unrelated uses of the words, or not a defect.

**E (evidence).** `yes` needs at least one of: a merged fix; a maintainer's confirmation (a
project developer reporting a defect in the project's own code counts); a runnable reproducer
or regression test in the thread or fixing PR. Analysis alone is `unclear`.

## Dimensions (only for `yes`)

Describe the state **at the instant the future is destroyed**, for the defect **as it manifests
in the report**.

### holder: where the responsibility or progress lives at that instant (one value)
- `dropped_future`: inside the destroyed future. **A value taken out of shared state
  (`Option::take()`, popped from a queue, moved out of an `Arc`) and not yet put back lives
  here**, even though the defect is observed through the shared state later.
- `shared_object`: in an object that outlives the future and is used again (a connection or
  stream mid-protocol, a struct behind a lock, a barrier, a flag), and was modified in place.
- `other_task`: another task that keeps running.
- `blocking_thread`: work on the blocking pool or a plain OS thread.
- `kernel_or_remote`: an operation already handed to the kernel or a remote peer.
- `runtime`: runtime/library-internal machinery only (scheduler, wait queues, waker lists,
  semaphore permits).

### phase: decided by the first question answered "yes" (one value)
1. Did the future's work already produce an effect or a result that nobody has recorded or
   taken ownership of (data delivered but not marked as sent; a snapshot persisted before its
   cleanup guard exists; an S3 upload created)? → `effect_unrecorded`
2. Was completion recorded before the work actually happened (a "finished" flag set before the
   awaited finish work; a state marked Complete before the terminal message is sent)?
   → `recorded_ahead`
3. Was something taken or begun and not finished, **including work still in flight in a
   thread, the kernel or a peer that completes only after the cancellation**? → `in_progress`
4. Nothing had been taken or begun. → `not_started`

Runtime-internal defects with no user-level operation: `n/a`.

### after: who uses the affected state next, in the report's failure scenario (one value)
- `retry`: the same caller reissues the same operation (next loop iteration, retry of a request).
- `reuse`: the same caller uses the same object for a different operation.
- `others`: other callers or tasks depend on it (waiters on a shared lock/entry/queue, other
  requests on a pooled connection, the peer).
- `teardown`: the object is closed / cleaned up, or the process is shutting down.
- `nothing`: nothing uses it afterwards; the defect is in the destruction itself (a panic or
  leak in `Drop`).
- `unknown`.

### violation: which contract is broken
`violation` holds **one** primary value: the one through which the defect manifests in the
report. Optional further values go in `violation_extra` (`;`-separated).
`loss` | `duplication` | `reorder` | `corruption` (byte stream / framing) | `mismatch`
(response paired with the wrong request) | `invariant` (object left violating its own
invariant) | `leak` | `effect_after_close` | `liveness` (hang, deadlock, cannot exit) |
`panic` | `memory_safety` | `collateral_failure` (unrelated operations fail).

### outlives
`yes` if work continues after the future is destroyed (blocking pool, kernel, another task).

### cancel_source (one value)
`select` (select!/select combinators and fail-fast joins) | `timeout` | `abort` |
`owner_dropped` (the owner drops it: server on client disconnect, container cleared, explicit
`drop`) | `shutdown` | `other`.

### layer
`app`: code that uses async primitives. `library`: the implementation of a reusable async
library or runtime.

### found_by
`failure` only if the report describes an occurrence observed **before** the analysis (logs,
incident, failing test in CI, user-visible symptom). Reproducers written to demonstrate an
analysis, audits and code review: `review`.

## Fields filled by the project, not by annotators
- `reproduced`: `executed` (reproduced with Dropwise against the original project at a pinned
  version) | `modelled` (reproduced in a reduced model of the same mechanism) | `failed`
  (attempted, did not reproduce) | `not_attempted`.
- `source_cluster`: reporter account when several reports come from one account or batch.
- `evidence`: `fix_merged` | `maintainer` | `reproducer` (the strongest available).

## Annotation rules
1. Independence: fill only your own sheets; do not open other annotators' sheets or model
   labels (`data/labels.jsonl`, `data/annotate.csv`, `labels/batch*.py`, `labels/dims*.py`,
   `labels/human/`, the other annotator's `labels/verify/*` sheets,
   `labels/verify/adjudicated_*.csv`).
2. Read the issue/PR, all comments, and the fixing PR (and its diff when needed).
3. `notes` is required: one or two sentences with the key evidence and why the holder/phase
   values were chosen. Cases the codebook does not cover start with `codebook-gap:`.
4. Values must come from the sets above.
