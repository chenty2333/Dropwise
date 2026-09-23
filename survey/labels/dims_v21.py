# Revisions for codebook v2.1 (2026-09-23), applied after dims_v2.py.
# 1. Evidence rule: `yes` needs a fix, maintainer confirmation, or a reproducer/regression test.
#    Checked via issue state, maintainer comments and merged cross-references (gh API).
# 2. `phase` is judged at the instant of cancellation: work completing later is
#    acquired_uncommitted with outlives=yes.

H, T, A = "hyperium/hyper", "tokio-rs/tokio", "tokio-rs/axum"
H2, O = "hyperium/h2", "oxidecomputer/omicron"
PG, MS, QD = "cloudflare/pingora", "meilisearch/meilisearch", "qdrant/qdrant"
RW, VE, PK, DN = "risingwavelabs/risingwave", "vectordotdev/vector", "paritytech/polkadot-sdk", "denoland/deno"

NO_EVIDENCE = "v2.1: no fix, maintainer confirmation, or reproducer found"


def unclear(why=NO_EVIDENCE):
    return dict(relevant="unclear", confidence="medium", notes_append=why)


def late(why="v2.1: work completes after cancellation, so acquired_uncommitted"):
    return dict(phase="acquired_uncommitted", notes_append=why)


DIMS = [
    # evidence
    (H, 3906, unclear("v2.1: maintainer asked for clarification; closed not planned")),
    (H2, 907, unclear("v2.1: closed not planned, no maintainer response")),
    (T, 8112, dict(relevant="no", confidence="medium",
                   notes_append="v2.1: maintainers: not reachable with current APIs")),
    (T, 8258, unclear("v2.1: PR unmerged; maintainer questioned whether the fix belongs in mio")),
    (A, 2705, unclear("v2.1: PR unmerged, no maintainer response")),
    (PG, 931, unclear()),
    (PG, 933, unclear()),
    (MS, 6508, unclear()),
    (QD, 9745, unclear()),
    (QD, 9532, unclear("v2.1: maintainer could not reproduce")),
    (RW, 26176, unclear()),
    (RW, 26180, unclear()),
    (RW, 26179, unclear()),
    (RW, 26172, unclear()),
    (VE, 24670, unclear()),
    (PK, 11013, unclear()),
    # phase at the instant of cancellation
    (T, 7979, late()),
    (T, 5535, late()),
    (T, 3711, late()),
    (O, 10204, late()),
    (DN, 35601, late()),
]
