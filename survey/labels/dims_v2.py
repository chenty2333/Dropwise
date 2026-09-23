# Codebook v2 dimensions for rows labelled in batch1/batch2, plus corrections.
# Applied on top of the batch labels by apply.py (fields here win).
from batch1 import O, P, H, H2
from batch2 import T, A, Q


def d(holder, phase, after, violation, outlives, tags, **extra):
    return dict(holder=holder, phase=phase, after=after, violation=violation,
                outlives=outlives, tags=tags, **extra)


# Corrections after review (2026-09-23):
# - "zombie operation" rows were marked injectable=no; Dropwise's settle phase and
#   after_settle checks reproduce that shape (tests/semantics.rs), so they are yes/unknown.
# - futurelock is not a destroyed future: relevant=adjacent.
ZOMBIE_FIX = "injectable corrected: late work observable with settle phase"

DIMS = [
    # --- app layer ---
    (O, 3356, d("dropped_future", "acquired_uncommitted", "retry", "loss", "no", "wrapper")),
    (P, 434, d("dropped_future", "acquired_uncommitted", "retry", "loss", "no", "wrapper")),
    (P, 650, d("dropped_future", "delivered_unrecorded", "retry", "duplication", "no",
               "wrapper;context")),
    (O, 3579, d("shared_object", "acquired_uncommitted", "reuse", "invariant", "no", "context")),
    (O, 3140, d("shared_object", "delivered_unrecorded", "retry", "invariant", "no", "context")),
    (O, 3345, d("dropped_future", "acquired_uncommitted", "unknown", "invariant", "no", "context")),
    (O, 10204, d("blocking_thread", "committed_unowned", "close", "effect_after_close", "yes",
                 "context", injectable="yes", notes_append=ZOMBIE_FIX)),
    (T, 6493, d("shared_object", "acquired_uncommitted", "retry", "invariant", "no", "single_api")),
    (T, 6877, d("shared_object", "acquired_uncommitted", "retry", "corruption", "no", "single_api")),
    (T, 5285, d("shared_object", "acquired_uncommitted", "reuse", "mismatch", "no", "context")),
    (T, 5535, d("blocking_thread", "committed_unowned", "reuse", "loss", "yes", "context",
                injectable="yes", notes_append=ZOMBIE_FIX)),
    (T, 3711, d("blocking_thread", "committed_unowned", "retry", "leak", "yes", "other",
                injectable="yes", notes_append=ZOMBIE_FIX)),
    # --- library layer ---
    (H, 3995, d("dropped_future", "acquired_uncommitted", "none", "loss", "no", "wrapper",
                notes_append="consumer treats truncated body as complete")),
    (H, 4040, d("other_task", "acquired_uncommitted", "none", "leak", "yes", "other")),
    (H, 3199, d("dropped_future", "acquired_uncommitted", "none", "collateral_failure", "no",
                "wrapper")),
    (H, 3906, d("runtime", "n/a", "shutdown", "panic", "no", "other")),
    (H2, 907, d("runtime", "n/a", "none", "panic", "no", "other")),
    (H2, 138, d("shared_object", "acquired_uncommitted", "none", "liveness", "no", "other")),
    (H2, 546, d("runtime", "n/a", "shutdown", "liveness", "no", "other")),
    (T, 2318, d("blocking_thread", "acquired_uncommitted", "shutdown", "liveness", "yes", "other",
                injectable="unknown",
                notes_append="needs a check that the runtime can shut down; not modelled yet")),
    (T, 7979, d("kernel_or_remote", "committed_unowned", "none", "leak", "yes", "wrapper",
                injectable="yes", notes_append=ZOMBIE_FIX)),
    (T, 8112, d("runtime", "n/a", "none", "panic;memory_safety", "yes", "other")),
    (T, 8260, d("runtime", "acquired_uncommitted", "none", "liveness", "no", "other")),
    (T, 2685, d("kernel_or_remote", "committed_unowned", "none", "leak", "yes", "other")),
    (T, 3965, d("runtime", "n/a", "none", "leak", "no", "other")),
    (T, 3929, d("runtime", "n/a", "none", "memory_safety", "no", "other")),
    (T, 3672, d("runtime", "n/a", "none", "panic", "no", "other")),
    (T, 2340, d("runtime", "n/a", "none", "memory_safety", "no", "other")),
    (T, 1842, d("runtime", "n/a", "shutdown", "panic", "no", "other")),
    (T, 542, d("runtime", "n/a", "shutdown", "leak", "no", "other")),
    (T, 8258, d("kernel_or_remote", "committed_unowned", "none", "memory_safety", "yes", "other")),
    (Q, 1458, d("shared_object", "acquired_uncommitted", "none", "leak", "no", "other")),
    (A, 2705, d("other_task", "acquired_uncommitted", "none", "leak", "yes", "other")),
    # --- reclassified ---
    (O, 9268, dict(relevant="adjacent", holder="", phase="", after="", violation="liveness",
                   outlives="", tags="", notes_append="futurelock: future kept but not polled")),
]
