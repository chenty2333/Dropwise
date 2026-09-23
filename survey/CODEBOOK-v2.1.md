# Annotation codebook (v2.1, frozen for verification on 2026-09-23)

One row per candidate in `data/annotate.csv`. Leave a field empty if unsure and say why in
`notes`. v2 replaces the single-label `category` of v1 with orthogonal dimensions; the v1
labels survive as the multi-valued `tags` field.

## Scope: what counts as a cancellation defect

A *cancellation* here is the destruction of a future (or of the task owning it) before it
completes: a losing `select!` branch, an elapsed `timeout`, `abort()`, an explicit `drop`, a
disconnected client whose handler is dropped, runtime shutdown.

## relevant
- `yes`: a real defect whose trigger is a cancellation as defined above, fixed or confirmed by
  maintainers (or reproduced in the thread).
- `adjacent`: a related defect where the future is **not** destroyed, e.g. futurelock (a future
  kept alive but no longer polled while it holds a resource others need; Oxide RFD 609).
  Reported separately, not counted with `yes`.
- `dup`: same defect as another row (fix PR/commit of an issue, mirrored advisory); put the
  primary row's URL in `fix_url`.
- `unclear`: the cause cannot be established from the thread.
- `no`: docs, questions, feature requests, refactors, unrelated uses of the words.

Only `yes` rows enter the statistics.

## Orthogonal dimensions (for `yes` rows)

### holder: where the responsibility or progress lives when the future is destroyed
- `dropped_future`: locals of the destroyed future (a received message, a partially sent frame,
  a shared "connecting" task owned by one request).
- `shared_object`: an object that outlives the future and will be used again (a connection or
  stream mid-protocol, a struct behind a mutex, a barrier, a channel).
- `other_task`: another task that keeps running (a body-pipe task, a detached connection task).
- `blocking_thread`: work on the blocking pool or a plain OS thread (tokio::fs, stdin, DNS).
- `kernel_or_remote`: an operation already handed to the kernel or a remote peer (io_uring,
  overlapped I/O, a child process, a remote server).
- `runtime`: internal state of a runtime/library (scheduler, waker lists, semaphore permits).

### phase: how far the operation had got when it was destroyed
- `before_acquire`: nothing had been taken yet.
- `acquired_uncommitted`: something was taken or a state change begun, not completed or recorded.
- `committed_unowned`: at the moment of cancellation the effect has already happened or a
  result already exists, and nobody owns it any more (a snapshot persisted before its cleanup
  guard is registered; an S3 multipart upload already created). Work that only completes
  *after* the cancellation (a late fd, a byte consumed later by a blocking read) is
  `acquired_uncommitted` with `outlives=yes`.
- `delivered_unrecorded`: the effect was delivered but the bookkeeping that says so was not
  updated (so a retry repeats it).
- `recorded_undelivered`: the reverse: bookkeeping already says "done" but the effect was not
  delivered (so a retry skips it).
- `n/a`: runtime-internal defects where no user-level operation phase applies.

### after: what the caller does after the cancellation
`retry` (reissues the same operation) | `reuse` (keeps using the object for something else) |
`close` (closes / cleans up the object or its resources) | `shutdown` (the process or runtime is
exiting) | `none` (nothing further; the defect is in the drop itself) | `unknown`.

### violation: which contract is broken (multi-valued, `;`-separated)
`loss` | `duplication` | `reorder` | `corruption` (byte stream / framing) | `mismatch` (response
paired with the wrong request) | `invariant` (object left violating its own invariant) | `leak` |
`effect_after_close` | `liveness` (hang, deadlock, cannot exit) | `panic` | `memory_safety` |
`collateral_failure` (unrelated operations fail because of this cancellation)

### outlives: does work continue after the waiter is destroyed?
`yes` if the defect involves work that keeps running after the future is dropped (blocking pool,
kernel, another task); `no` otherwise. Not a defect by itself; it says where to look.

## Annotation rules (v2.1)

1. **Evidence.** `yes` needs a fix, a maintainer's confirmation, or a reproduction: a runnable
   reproducer or regression test in the thread or the fixing PR (by anyone). A plausible
   analysis without any of these is `unclear`, not `yes` or `no`.
2. **Instant of cancellation.** `phase` and `holder` describe the state at the moment the
   future is destroyed. What happens later goes into `outlives` and `notes`.
3. **One holder.** Choose the location of the responsibility or progress whose loss or
   misplacement directly causes the violation; mention other locations in `notes`; leave empty
   (with a note) if it cannot be decided. `runtime` is only for scheduler, wait-queue,
   waker-list and permit machinery, not for every library object.
4. **Injectable** is a judgement about the *current* Dropwise: you may build a scenario, mark
   targets, add obligations and `after_settle` checks, and replace an external service with an
   in-process fake when the defect's logic does not depend on that service's behaviour. You
   may not assume new tool features or change the defect's logic. If the defect depends on the
   external service's own behaviour, answer `unknown`. `yes` does not mean it was run.
5. **Duplicates** are judged within your own sheets only. The same URL is never a duplicate
   of itself. Different URLs describing one defect: keep the row with the strongest evidence
   as primary (an issue when evidence is equal) and mark the others `dup` with the primary's
   URL in `fix_url`.
6. **Independence.** Each annotator fills only their own sheets (A or B), without looking at
   the other annotator's sheets or any model label (`data/labels.jsonl`, `data/annotate.csv`,
   `labels/batch*.py`, `labels/dims*.py`).

## Other fields

### tags (v1 categories, multi-valued)
`single_api` (a cancel-unsafe operation used directly where it can be cancelled) | `wrapper`
(progress held only inside the destroyed future of a helper) | `context` (the caller's later use
assumes a clean state) | `other`.

### cancel_source
`select` | `timeout` | `abort` | `drop` | `shutdown` | `other`

### layer
`app`: the defect is in code that *uses* async primitives (application, service, internal helper).
`library`: the defect is in the implementation of a reusable async library or runtime.

### found_by
`failure` (observed misbehaviour, test failure, user report) | `review` (code reading, audit,
security review).

### injectable
Could Dropwise reproduce it, given a scenario around the fix (including the settle phase and
`after_settle` checks)? `yes` | `no` | `unknown`; explain `no`/`unknown` in `notes`.

### fix_url, notes, annotator, confidence
Fixing PR/commit when the row is not the fix; free text; who labelled; `high`/`medium`/`low`.

## Deprecated (v1)
`category` and `consequence` are kept for rows labelled before v2; use `tags` and `violation`.
