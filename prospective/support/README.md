# Cancellation target + successor combinations

`combine.rs` is experiment-local support. The adapter registers operations and
capabilities; `plan_cases`/`run_cases` own the combination loop.

## What the adapter declares

```rust
impl Adapter for BoltAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs;

    fn targets(&self) -> Vec<TargetOp> { /* registered targets */ }
    fn successors(&self) -> Vec<SuccessorOp> { /* registered successors */ }
    fn target_shape(&self, target: &TargetOp) -> Shape { /* object/payload slots */ }
    fn successor_shape(&self, successor: &SuccessorOp) -> Shape { /* object/payload slots */ }
    fn realize(
        &self,
        target: &TargetOp,
        successor: &SuccessorOp,
        object: Option<ObjectRelation>,
        payload: Option<PayloadRelation>,
    ) -> Result<SuccessorOp, String> { /* concrete successor or reason */ }
    fn expect(&self, case: &Case<TargetOp, SuccessorOp>) -> Expect { Expect::Atomic }
    fn scenario(&self, ctx: Ctx, case: Case<TargetOp, SuccessorOp>) -> CaseFuture<Obs> {
        /* one scenario for this case; rebuild isolated state */
    }
}
```

`Shape` has optional `object` and `payload` slots. A relation is expanded only
when both sides have the same `kind`. `None` means the operation has no such
capability, so the generator emits one case with that dimension `n/a` and a
`CASE-NOTE` instead of inventing a combination. `realize` must return the
concrete successor for the requested relation; the generator verifies the
realized shape and skips an inconsistent realization before execution.

## Adding one successor adds these combinations

The BOLT adapter registers `write`, `write+read`, and `read-tag`. With only the
first two, the generator makes 10 cases. Adding `read-tag` needs only its
descriptor, shape, `realize` arm and scenario arm. The generator then adds:

| target | added cases | object | payload |
|---|---|---|---:|
| `write(primary,1)` | `read-tag(primary)`, `read-tag(alternate)` | same, different | n/a |
| `checkout` | `read-tag(primary)` | n/a | n/a |

That is 13 cases total, with no change to the generator loop. The keyed target
gets both object relations; the keyless checkout target contributes one case
because there is no business object to vary.

If a new successor has a different payload kind, the payload dimension is not
expanded. The mysql adapter's `insert(row=2)` uses `mysql/row-value`, while the
target uses `mysql/query-value`; this yields one case per applicable target
with `payload=n/a` and this note:

```text
CASE-NOTE payload relation not expanded: target kind mysql/query-value != successor kind mysql/row-value
```

## Running, limit, replay

```rust
let runs = run_cases(&adapter, &config, 16);
runs.print();
```

Order is target registration order, successor registration order, then object
`same`/`different`, payload `same`/`different`. Each case calls
`explore_observed` separately, and `CaseRun` keeps the `Case` label next to the
existing `Report`. Applicable cases beyond the limit go to
`CaseRuns::plan.unrun`; they are not run and never count as passed.

```rust
let run = run_case(&adapter, &config, &case);
if let Some(plan) = run.first_violation_plan() {
    let replay = replay_case(&adapter, &config, &case, &plan)?;
}
```

`replay_case` reuses the case description and passes the ordinary `Plan` to
`replay_observed`.

No-service generation tests:

```bash
cargo test --offline --manifest-path prospective/phase3/db-clients/Cargo.toml \
  --test generation_logic
```

Opt-in live runs (the historical defaults are unchanged):

```bash
TARGETS=mysql_generate VARIANT=release bash prospective/phase3/run-db.sh
MODE=generate bash prospective/phase4-exploration/clients/bolt/run-round2.sh
```
