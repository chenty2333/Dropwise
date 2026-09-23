# Dataset: cancellation defects in async Rust (state of 2026-09-24)

This document records how the dataset was built, the denominators at every step, who
labelled what, measured agreement, how disagreements were resolved, and the known risks.
Labels live in `labels/human/` (verified), `data/labels.jsonl` (initial model labels) and
`labels/verify/` (all annotation sheets, adjudication files and scripts).

## 1. Collection

GitHub search (issues and PRs; commits in the first round only) over 27 repositories
(`repos.txt`: tokio, hyper, h2, axum, tracing, quinn, tonic/grpc-rust, omicron, propolis,
dropshot, Materialize, Neon, RisingWave, Databend, Vector, InfluxDB, Pingora, iroh,
rust-libp2p, polkadot-sdk, Qdrant, Meilisearch, Deno, redis-rs, reqwest, linkerd2-proxy);
candidates were found in 26 of them.

| Search round | Phrase set | Candidates |
|---|---|---|
| cancel | "cancel safe", "cancellation safety", "future is dropped", "futurelock", ... (15 phrases) | 1,101 (663 only here, 438 also found by symptom) |
| symptom | "lost message", "select! loop", "cancelled mid", "request was cancelled", "partial write", ... (30 phrases) | 2,716 only here |
| **total** | | **3,817** |

Dependency-bump PRs were filtered by title. Commit search was dropped after the first round
(secondary rate limits; in the first 369 labelled candidates every commit was a duplicate of
a PR or unrelated). Two symptom queries for neondatabase/neon failed and were re-run; the 22
late candidates went straight to full annotation (section 3, round 3).

## 2. Annotators

| Stage | Annotator(s) | Blind? |
|---|---|---|
| Initial labels (cancel round, 1,101 rows) | Claude Opus 5.5 session that also wrote the codebook ("model") | no |
| Round 1, codebook v2.1 | A: Claude Opus 5.5 (high); B: ChatGPT 6 Astra (xhigh) | yes |
| Rounds 2–3, codebook v3 | A: GPT 6 Sol (medium); B: Claude Opus 5.5 (low) | yes |
| Adjudication, evidence pass | the initial "model" session | no (chooses between A and B only) |

All annotators are language models; no human annotation was performed. The annotator pair
changed between round 1 and rounds 2–3, so differences in kappa between rounds mix the
codebook revision with the change of annotators.

## 3. Verification rounds and denominators

**Round 1 (v2.1).** `positives_{A,B}.csv`: all 109 rows the model labelled yes / unclear /
adjacent. `negatives_{A,B}.csv`: 60 rows drawn from the 950 model negatives, stratified by
(model confidence, repository) with at least one row per stratum (not proportional).

**Round 2 (v3).** `v3_{A,B}.csv`: the 80 rows verified as defects in round 1, re-annotated on
the v3 dimensions. `triage_{A,B}.csv`: all 2,694 symptom-round candidates not yet labelled,
keep/drop from title and excerpt (reading the source when unclear).

**Round 3 (v3).** `full_{A,B}.csv`: 250 rows = the union of triage `keep` (228) + 22 late neon
candidates, fully annotated (relevant and dimensions).

| Round | Rows | Unit | relevant: agreement / kappa |
|---|---|---|---|
| 1 positives | 109 | model-flagged | 89.9% / 0.78 |
| 1 negatives | 60 | sampled model negatives | 75.0% / 0.36 |
| 2 triage | 2,694 | symptom candidates | 94.1% / 0.44 (keep/drop) |
| 3 full | 250 | triage keep + late | 83.6% / 0.64 |

Dimension agreement (Cohen's kappa, A vs B):

| Field | Round 1 (v2.1, n=75) | Round 2 (v3, n=80) | Round 3 (v3, both-yes n=16) |
|---|---|---|---|
| holder | 0.79 | 0.72 | 0.74 |
| phase | 0.46 | 0.75 | 0.48 |
| after | 0.58 | 0.56 | 0.60 |
| violation (v2.1 set / v3 primary) | 0.44 | 0.74 | 0.85 |
| outlives | 0.89 | 0.84 | 0.76 |
| cancel_source | 0.73 | 0.79 | 0.56 |
| layer | 0.97 | 0.89 | 0.87 |
| found_by | 0.73 | 0.79 | 0.59 |
| tags (v2.1 only) | 0.19 | removed | — |
| injectable (v2.1 only) | 0.30 | removed | — |
| violation_extra (v3) | — | 0.30 | — |

Round 3 dimension kappas rest on 16 rows and are shown for completeness only.
`tags`, `injectable` and `violation_extra` are not used in any analysis. `after` stays below
0.6 in every round: report it as descriptive, with its kappa.

Initial model labels vs round-1 consensus (rows where A = B): of the model's `yes`, 62 of 67
were confirmed; 13 rows the model had downgraded to `unclear` under the v2.1 evidence rule
were judged `yes` by both annotators (the model's downgrade was too strict).

## 4. Adjudication

Only disagreements were adjudicated, by choosing between A's and B's answers using their
notes and the sources; each decision has a written rationale.

| File | Content |
|---|---|
| `adjudicated_relevant.csv` | 26 round-1 + 41 round-3 `relevant` disagreements |
| `adjudicated_v3.csv` | 121 dimension cells in 66 rows (`violation_extra` not adjudicated; kept only where A = B) |
| `dedup.csv` | 8 cross-round duplicates (fix PR vs issue in different rounds); 3 of them found only in the evidence pass |

Scope rules introduced during adjudication and written into codebook v3: a dropped non-future
handle is `adjacent`; a future dropped before its first poll because of a missing `.await`
is `no`.

Seven defects have dimensions from one annotator only (the other judged the row not a
defect; adjudication decided `yes`).

## 5. Final dataset

`labels/human/relevant.csv` (`relevant`), `labels/human/v3.csv` (dimensions),
`labels/human/evidence.csv` (evidence). `./survey.py stats --verified` reproduces the numbers.

- 419 rows verified by two annotators → **97 defects** (plus 46 adjacent, 11 unclear, 20 dup,
  245 no). 75 came from round-1 positives, 19 from round 3, 3 from the round-1 negative sample.
- Found by the symptom round only: 21 defects that the cancellation vocabulary missed.
- Layer: app 63, library 34.
- holder: dropped_future 37, shared_object 24, runtime 13, other_task 8, blocking_thread 8,
  kernel_or_remote 7. phase: in_progress 62, n/a 15, effect_unrecorded 14, recorded_ahead 4,
  not_started 2. outlives = yes: 32.

### Evidence (evidence pass, `labels/evidence/`)

Each row was checked against the raw facts collected from GitHub (`facts.jsonl`: PR merge
state and merger, issue state, cross-referenced PRs and their titles, maintainer comments,
labels) and read by hand; a link was counted as a fix only when the linked change addresses
this defect (e.g. docs-only changes, and a tokio#542 → tokio#649 link whose relation could
not be verified, were not counted).

| evidence (strongest) | Defects |
|---|---|
| fix_merged: a merged change fixes this defect | 66 |
| maintainer: a maintainer or project developer confirms the mechanism (comment, own report, or an `accepted` label) | 22 |
| reproducer only: a runnable reproducer or regression test, no fix and no maintainer | 9 |

Flags: `reporter_only` (7: the reporter's own analysis/test is the only evidence; all seven
are audit-style reports, 4 in RisingWave, 2 in Pingora, 1 in Meilisearch, with at most a
`bug` label; for pingora#931 our own executed reproduction on the latest release adds
evidence outside the thread) and `contested` (2: qdrant#9532, maintainer could not reproduce, label `parked`;
vector#21657, maintainer calls the behaviour intended during shutdown). Analyses should be
reported with and without these 9.

Reproduction status (`labels/human/reproduced.csv`): executed 6, modelled 5 (see `../repro/`).

## 6. Risks and limitations

1. **Model-only negatives in the cancel round.** 890 of the 950 rows the model labelled `no`
   were never seen by a second annotator. In the 60-row sample, 3 turned out to be defects
   (5%; the sample is stratified, so this is only indicative). If the rate held, the 890
   rows would hide on the order of tens of defects (exact 95% interval for 3/60 in a
   population of 949: 1.2%–13.6%). **This is the largest known gap.** Remedy: double triage
   of the 890 rows by A and B, as for the symptom round.
2. **Triage drops.** 2,466 symptom candidates were dropped by both annotators from title and
   excerpt and never read in full. Yield of kept rows: both keep 10/70 (14%), B-only 6/40
   (15%), A-only 3/118 (2.5%). The double-drop miss rate is unmeasured; a simple random audit
   of ~300 double-drops would bound it near 1% if none are found.
3. **Keyword sampling.** The corpus consists of reports that use either cancellation
   vocabulary or chosen symptom phrases, in 27 projects chosen for heavy async use. Shares
   describe this corpus, not the Rust ecosystem.
4. **Report clusters.** Several 2026 reports share one reporter account and an audit-style
   format; they are flagged via `evidence_flag` and should be reported separately.
5. **Annotators are models; the adjudicator is the initial annotator.** Adjudication only
   chooses between the two blind answers, with written rationales, but it is not independent.
6. **Contracts in reproductions were written after reading each report** (see
   `../repro/README.md`); they show expressiveness, not discovery.
