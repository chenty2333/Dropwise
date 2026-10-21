# Field-level observation contracts

Additive method extension to `research/python-retro/dropwise_py/`.  It does not
change `engine.py` or any existing case; `explore_observed`/B1/B2/B3/M1 keep
their behaviour.  New code: `dropwise_py/fields.py`, `tests/test_fields.py`,
and a synchronous consumer in `prospective/offline-discovery/python/pgsync/case.py`.

## 1. The blind spot in the single-blob observation

The engine decides M1 by comparing one observation value `{"survivor": ...,
"observer": ...}` against a pre and a post calibration.  That value fuses
independent effects and carries no per-field coverage statement.  Two failures
follow:

* **Legitimate mixtures look like violations.**  If effect A completed and
  effect B did not, the pair is neither the pre pair nor the post pair, so the
  report says `Neither` — even when the caller's contract is atomic for A and
  completed for B.  The blob cannot express two expectations in one run.
* **A degenerate effect reads as clean.**  If a required effect is identical in
  pre and post (for example, an observer field that is always empty), the blob
  can still equal `post` because some *other* effect moved.  The run is reported
  as a pass even though the scenario never observed that effect.

Both are observable in the current code: `explore_observed` only appends a note
when `pre == post`, and a required field cannot be named at all.

## 2. The contract

An observation is a dict (nested dicts are flattened to dotted names).  A
contract is a list of `Field`s:

| field kind | meaning | judging |
|---|---|---|
| `EFFECT` | an externally visible effect | compared with pre and post; identical calibration is `Degenerate` |
| `STATE` | an invariant/guard | calibrations must agree; the trial must equal that agreed value, else `Violation` |
| `expect` (per field) | `Atomic`, `Completed`, `Undone` | an effect outside its own expectation is `Violation` |

Statuses: `Pre`, `Post`, `Mixed` (two effect fields disagree), `Holds` (state
guard), `Neither`, `Unknown` (unstable calibration), `Degenerate` (no
discriminating power), `Uncovered` (required field absent), `Violation`.

`judge_trial(contract, pre_samples, post_samples, observation)` is synchronous
and uses only plain dicts; `explore_fields(...)` is the asyncio driver, reusing
the engine's scenario lifecycle, cancellation plans, and two-sample calibration.

A trial is not "clean" unless the effect fields are covered and non-degenerate.
`FieldJudgment.coverage_complete` is false for `Uncovered`, `Degenerate`, and
`Unknown`; the report lists those separately.  This is the main methodological
change: *absence of a violation is not a pass when the scenario had no
discriminating power.*

## 3. What it changes on a real trial

The same sync entry-wait execution, judged by the same `judge_trial`:

| field | kind | pre | post | trial | status |
|---|---|---|---|---|---|
| `target.visible` | effect, atomic | false | true | false | Pre (allowed) |
| `successor.visible` | state | true | true | false | **Violation** |
| `client.clean` | state | true | true | false | **Violation** |
| `server.clean` | state | true | true | false | **Violation** |

Overall: `Violation`, coverage complete.  In the pre-send state (`D`) the same
judge gives `server.clean: Holds` and `client.clean: Violation` — the server is
IDLE, and only the local counter is wrong.  That is exactly the distinction the
pool's `transaction_status`-based reset misses (see `RESULTS-sync-signal.md`
§4.5).  The field report localizes the failure instead of saying "the blob is
neither pre nor post".

This is a *diagnosis* improvement, not a new bug: the old blob also ends in a
violation for these trials.  The field contract adds the reason and the coverage
statement.

Negative control: `entry_normal` (no interruption) is judged `Post` with
`successor.visible`, `client.clean` and `server.clean` all `Holds`, 2/2.  The
contract does not flag a normal execution; the violation rows are specific to
the interrupted states.

## 4. A constructed case where the old blob is wrong

`tests/test_fields.py::test_per_field_mixed_allowed_but_blob_is_neither` uses a
fake scenario with two independent effects: `a` completes before the first
suspension, `b` only after it.  At the cut after `a`:

* contract with `a: Atomic`, `b: Atomic` → overall `Mixed`, no violation;
* contract with `b: Completed` → `Violation`;
* the legacy `explore_observed` blob on the same trial → `Neither` (a violation
  under its atomic expectation).

This is a constructed contract, not a discovered library bug.  It shows the
expressive difference: the field contract can say what the caller actually
requires per effect; the blob has one expectation for all of them.

The same tests pin `Uncovered` (a required `observer.rows` field absent),
`Degenerate` (an always-empty observer field), a `STATE` guard violation, an
unstable calibration (`Unknown`), and dotted flattening.  13 tests, no service
or network needed:

```sh
cd research/python-retro && python3 -m unittest tests.test_fields -v
```

## 5. Migration to the synchronous target

`pgsync/case.py fields` runs the same `judge_trial` on sync psycopg trials.  No
asyncio import is needed by the judge; the contract is:

```python
Field("target.visible", EFFECT, Atomic)
Field("successor.visible", STATE)
Field("client.clean", STATE)
Field("server.clean", STATE)
```

Observed results for `entry_wait`, `entry_pre_send`, and `exit_pre_commit`
(2 trials each, both retry and survivor): overall `Violation` in all six cells;
`target.visible` is `Pre` (allowed); `server.clean` holds only in D; the other
fields are violations.  The same numbers are in the raw `fields.jsonl`.

This is a real method migration: the judging code is shared between an async
cancellation engine and a sync signal harness, and the sync target was not used
to design the field contract.  It is not a migration of the *injection* method
(sync signals vs asyncio cancellation); the two targets have different
reachable states, which is the point of §3 in the results report.

## 6. Limits

* Fields are **declared**, not discovered.  A missing field is caught only if
  the contract names it; the contract author can still forget an effect.  The
  method improves coverage judgment, not completeness of the field set.
* Effects are assumed independent.  If two effects are really one effect with
  an internal order, judging them separately can legitimate a partial outcome
  that the caller did not want.  The `expect` per field is the place to encode
  that, and it is a human decision.
* Calibration is still two samples; unstable fields become `Unknown`, which is
  honest but not a measurement of how often the scenario is unstable.
* `Mixed` is allowed only when each field's own expectation allows its status;
  there is no global cross-field invariant (e.g., "at most one effect may be
  post").  Such invariants would need a separate predicate layer.
* The implementation is Python-only.  The Rust engine has not been changed; a
  port would need to map the observation type and the report/verdict plumbing.

## 7. Next

1. Port `Field`/`judge_trial` to Rust and use it in the engine's M1 report; keep
   the existing single-observation API as a compatibility wrapper.
2. Auto-derive effect fields from resource identity (the observer's rows) and
   state fields from the handle's public + internal facts, so the contract is
   not hand-written per case.
3. Use the coverage statement to reject scenarios that currently pass M1 only
   because one of their effects is degenerate; re-run the round-3/4 cases and
   report which passes were actually uncovered.
