# Cancellation target + successor combinations

`combine.rs` is experiment-local support. The adapter registers operations and
their typed parameter slots; `plan_cases`/`run_cases` own the combination loop.

## What the adapter declares

```rust
impl Adapter for BoltAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs; // PartialEq + Debug + Send + 'static + Serialize

    fn targets(&self) -> Vec<TargetOp> { /* registered targets */ }
    fn successors(&self) -> Vec<SuccessorOp> { /* registered successors */ }
    fn expect(&self, case: &Case<TargetOp, SuccessorOp>) -> Expect { Expect::Atomic }
    fn scenario(&self, ctx: Ctx, case: Case<TargetOp, SuccessorOp>) -> CaseFuture<Obs> {
        /* one scenario for this case; rebuild isolated state */
    }
}
```

Operations declare their parameters once, through `ParamSlots`. The same
traversal is used to extract the target/successor `Shape` and to rewrite a
successor for a requested relation:

```rust
impl ParamSlots for TargetOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            TargetOp::Checkout => Ok(()),
            TargetOp::Write { node, value } => {
                visitor.visit(SlotRole::Object, node)?;
                visitor.visit(SlotRole::Payload, value)
            }
        }
    }
}

TargetOp::Write {
    node: Param::new("bolt/node", NodeRole::Primary),
    value: Param::new("bolt/write-value", TARGET_V),
}
```

`Param<T>` carries a stable logical `domain`, the concrete typed `value`, and
adapter-declared `alternatives`. `domain` is not a Rust type or a display
string: equal primitive types and equal display strings do not establish a
shared identity. A relation is expanded only when both sides declare the same
domain, and an in-process `TypeId` check rejects two different Rust types that
claim the same domain label. `None` means the operation has no such capability,
so the generator emits one case with that dimension `n/a` and a `CASE-NOTE`.

Parameter realization is shared:

- `Same` copies the target's typed value into the successor.
- `Different` first keeps the successor's seed when it is already unequal;
  otherwise it picks the first declared alternative unequal to the target.
- No unequal value, duplicate alternatives, duplicate roles, a domain declared
  twice in one operation, or an incompatible Rust type for one domain produces
  an explicit skipped pair, never a panic or a silent passing case.
- Realization is followed by the existing relation check. Applicability and
  `Expect` remain adapter-owned; a declared representative is not proof that
  the protocol/context accepts it.

The BOLT adapter's target/successor `Display` and registration order are
unchanged. The keyless `checkout` target still gets one base case per successor;
the mysql `SELECT 12345` payload is not treated as an object key.

## Adding one successor adds these combinations

The BOLT adapter registers `write`, `write+read`, and `read-tag`. With only the
first two, the generator makes 10 cases. Adding `read-tag` needs only its
operation variant, one `visit_slots` arm and its scenario arm. The generator then
adds:

| target | added cases | object | payload |
|---|---|---|---:|
| `write(primary,1)` | `read-tag(primary)`, `read-tag(alternate)` | same, different | n/a |
| `checkout` | `read-tag(primary)` | n/a | n/a |

That is 13 cases total. The keyed target gets both object relations; the keyless
checkout target contributes one case because there is no business object to
vary. If a new successor has a different payload domain, the payload dimension
is not expanded. The mysql adapter's `insert(row=2)` uses `mysql/row-value`,
while the target uses `mysql/query-value`; this yields one case per applicable
target with `payload=n/a` and this note:

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
`replay_observed`. Real I/O replay is a retry with the same observation policy,
not a claim of deterministic replay.

No-service tests:

```bash
cargo test --offline --manifest-path prospective/phase3/db-clients/Cargo.toml \
  --test generation_logic
cargo test --offline --manifest-path prospective/phase3/db-clients/Cargo.toml \
  --test diagnostics_logic
cargo test --offline --manifest-path prospective/phase3/db-clients/Cargo.toml \
  --test mysql_generate mysql_plan_without_service
(cd prospective/phase4-exploration/clients/bolt/round2/original && \
  cargo test --offline --test generate bolt_plan_without_service)
(cd prospective/phase4-exploration/clients/bolt/round2/fixed && \
  cargo test --offline --test generate bolt_plan_without_service)
```

The last three run the real adapters' plan code without a database or server;
they do not execute scenarios.

Opt-in live runs (the historical defaults are unchanged):

```bash
TARGETS=mysql_generate VARIANT=release bash prospective/phase3/run-db.sh
TARGETS=mysql_generate VARIANT=patched bash prospective/phase3/run-db.sh
MODE=generate bash prospective/phase4-exploration/clients/bolt/run-round2.sh
```

`VARIANT=patched` is the guard/recovery configuration for mysql: the test checks
`Conn::is_disconnected()` and replaces the handle before the successor. It is a
separate caller-side recovery configuration and is printed as such.

## Structured observation diagnostics

Diagnostics are opt-in and read-only:

```rust
let runs = run_cases_with(&adapter, &config, 16, Diagnostics::On);
runs.print();
```

`DiagnosticObs<O>` wraps the original observation. Its `PartialEq` delegates to
`O`'s equality and its `Debug` delegates to `O`'s debug output, preserving the
engine's verdict and calibration policies. Diagnostic work can affect real-I/O
timing, so separate runs need not have identical coverage or counts. Only when
the original comparison is unequal does the wrapper record a structured
difference. `ObservationComparison` keeps both sides; for the engine's
`calibration == actual` checks the right side is the trial observation.
If unequal original observations serialize to indistinguishable fields, diagnostics
explicitly report that limitation. Field differences are individual comparisons
(including calibration comparisons), not standalone violations: an Atomic trial
can differ from pre and still match post.

`diagnose.rs` uses `serde::Serialize` as the traversal protocol and records the
Serde data model in a small value tree. The field paths distinguish:

- `Option`: `None` vs `Some(...)` at the field path, descending into `Some`;
- `Result`/enums: variant mismatch at the variant's path, or `.Variant` then
  the concrete field for a same-variant difference;
- integers, including `i128`/`u128`, and floats by bit pattern;
- sequences: order, index, length and multiplicity;
- struct fields and map keys (maps are compared by key, not container order);
- newtype and tuple/struct variants.

It is not a general reflection registry. The serializer sees only what a type's
`Serialize` implementation emits. Transparent/untagged/custom implementations
can still hide information; that limitation is documented rather than worked
around. A serialization error is recorded as `CASE-DIAG-ERROR` / a
`DiagnosticLog` error and never becomes an empty equal observation.

The ad hoc values that motivated the choice are:

- `serde_json::Value` maps `None` to `null`, so `Some(())` and `None` are not
  distinct, and JSON needs an extra feature for 128-bit integers.
- `serde_value::Value` 0.7 has no explicit enum-name/variant or 128-bit integer
  nodes; a unit variant becomes a bare string and a newtype variant becomes a
  map keyed by the variant name.

Neither is used as a lossless drop-in. The small tree in `diagnose.rs` is the
bounded alternative, not a claim that Serde can see Rust fields a custom
implementation omits.

### Identity annotations are neutral

An adapter may return `IdentityBinding`s from observed fields (for example the
normalized BOLT node tag). They keep the raw observed value and the target and
successor labels, and use only:

```text
MatchesTarget  MatchesSuccessor  MatchesBoth  Unmatched
```

`MatchesBoth` is exactly what a same-object case can produce: the observation
cannot distinguish the target from its successor. That ambiguity is shown, not
resolved into a verdict. Matching the target is not automatically a violation,
because `Expect::Atomic` legitimately allows a completed target effect. Equality
is not causal provenance. Identity annotations never become a second oracle;
the original `PartialEq` and the existing engine remain the only verdict source.

### Deliberate limits

- No learned stability mask and no automatic deletion of "dynamic" fields. A
  value that varies during calibration may still be the wrong value; unstable
  calibration stays `Unknown`.
- Two warmups do not replace plan-specific pre/post/mixed calibration.
- No parameter shrinking, random generation, longer operation sequences, or
  heuristic relation mining.
- No macro crate, reflection registry, public Dropwise API, or new workspace
  crate. The helpers stay under `prospective/support/`.
- No identity-based verdict, no "must come from successor" oracle, and no
  replacement of an observed value by an expected input.
- Plan replay is a real-I/O retry, not deterministic replay.

## Current implementation status

- `combine.rs` contains the typed slot traversal, shared realization and the
  read-only diagnostic wrapper. `diagnose.rs` contains the Serde tree, field
  diff and neutral identity matching.
- The mysql direct and BOLT pooled adapters no longer implement parallel
  `target_shape`, `successor_shape` and `realize` matches. They keep their
  operation enums, registration, scenarios, observers, applicability and
  `Expect`.
- The fake adapter tests cover generation order, relation collapse, limit,
  unrun/skip, replay, representative selection, invalid/ambiguous declarations,
  domain type conflicts, `Unknown` and serialization failure with diagnostics
  off/on. `diagnostics_logic` covers the value tree.
- Real-adapter plan tests check the concrete mysql 5-case and BOLT 13-case
  plans, including BOLT same-object identity ambiguity, without a service.
- Bounded live checks were run with the existing runners; exact violation
  counts are not required to match across real-I/O runs. Compare case
  semantics, realized cuts, verdict categories and coverage separately.

## Remaining manual work

The adapter still chooses target points and observers, writes resource setup and
cleanup, maps logical roles to physical keys, declares candidate
representatives, and decides which observed fields are worth binding
(`identities`). Diagnostics can point at an emitted field; they cannot prove
that a field was emitted losslessly or that an equal value came from the intended
request. Unordered collection semantics beyond keyed maps, automatic
normalization, masks, shrinking and longer sequences remain separate follow-ups
driven by demonstrated need.

References checked for this design: [Serde data model](https://serde.rs/data-model.html),
[`serde_value::Value`](https://docs.rs/serde-value/0.7.0/serde_value/enum.Value.html),
[`TypeId`](https://doc.rust-lang.org/std/any/struct.TypeId.html).
