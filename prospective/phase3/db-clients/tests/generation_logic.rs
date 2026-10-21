//! No-service checks for the shared target/successor combination generator.
//!
//! The main adapter is intentionally fake: it records what the shared layer
//! asked for and returns the requested relation without touching a server. The
//! tests cover generation order, dimension collapse, applicability, the
//! execution limit, replay, typed slot errors, and the read-only diagnostic
//! switch.

#[path = "../../../support/combine.rs"]
mod combine;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use combine::{
    Adapter, Case, CaseFuture, Diagnostics, ObjectRelation, Param, ParamSlots, PayloadRelation,
    SlotRole, SlotVisitor, plan_cases, replay_case, replay_case_with, run_cases, run_cases_with,
};
use dropwise::{Config, Ctx, Expect};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Keyed {
        key: Param<String>,
        value: Param<i64>,
    },
    Keyless,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Keyed { key, value } => {
                write!(f, "target-keyed({},{})", key.value, value.value)
            }
            Target::Keyless => write!(f, "target-keyless"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Successor {
    Keyed {
        key: Param<String>,
        value: Param<i64>,
    },
    Keyless,
    Foreign {
        key: Param<String>,
        value: Param<i64>,
    },
    Refused,
}

impl fmt::Display for Successor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Successor::Keyed { key, value } => {
                write!(f, "succ-keyed({},{})", key.value, value.value)
            }
            Successor::Keyless => write!(f, "succ-keyless"),
            Successor::Foreign { key, value } => {
                write!(f, "succ-foreign({},{})", key.value, value.value)
            }
            Successor::Refused => write!(f, "succ-refused"),
        }
    }
}

impl ParamSlots for Target {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            Target::Keyed { key, value } => {
                visitor.visit(SlotRole::Object, key)?;
                visitor.visit(SlotRole::Payload, value)?;
                Ok(())
            }
            Target::Keyless => Ok(()),
        }
    }
}

impl ParamSlots for Successor {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            Successor::Keyed { key, value } | Successor::Foreign { key, value } => {
                visitor.visit(SlotRole::Object, key)?;
                visitor.visit(SlotRole::Payload, value)?;
                Ok(())
            }
            Successor::Keyless | Successor::Refused => Ok(()),
        }
    }
}

/// The observation intentionally contains only the realized case parameters.
/// It does not contain the target's completion/cancellation control result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct Obs {
    target: String,
    successor: String,
    object: Option<ObjectRelation>,
    payload: Option<PayloadRelation>,
}

struct Fake {
    runs: Arc<AtomicUsize>,
}

impl Fake {
    fn new() -> Self {
        Self {
            runs: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn key(value: impl Into<String>) -> Param<String> {
        Param::new("test/node", value.into())
    }

    fn value(value: i64) -> Param<i64> {
        Param::new("test/value", value)
    }
}

impl Adapter for Fake {
    type Target = Target;
    type Successor = Successor;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![
            Target::Keyed {
                key: Fake::key("k"),
                value: Fake::value(1),
            },
            Target::Keyless,
        ]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            Successor::Keyed {
                key: Fake::key("other").with_alternatives(["alternate".to_string()]),
                value: Fake::value(9).with_alternatives([2]),
            },
            Successor::Keyless,
            Successor::Foreign {
                key: Param::new("test/other-node", "fk".to_string()),
                value: Param::new("test/other-value", 99),
            },
            Successor::Refused,
        ]
    }

    fn applicable(&self, case: &Case<Self::Target, Self::Successor>) -> Result<(), String> {
        match case.successor {
            Successor::Refused => Err("test adapter refuses this successor".into()),
            _ => Ok(()),
        }
    }

    fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
        Expect::Atomic
    }

    fn scenario(
        &self,
        ctx: Ctx,
        case: Case<Self::Target, Self::Successor>,
    ) -> CaseFuture<Self::Obs> {
        let runs = Arc::clone(&self.runs);
        Box::pin(async move {
            runs.fetch_add(1, Ordering::SeqCst);
            // One Pending boundary for the engine to cut. The result is the
            // target's control outcome and is deliberately not observed.
            let _ = ctx
                .target(async {
                    tokio::task::yield_now().await;
                    0i64
                })
                .await;
            Ok(Obs {
                target: case.target.to_string(),
                successor: case.successor.to_string(),
                object: case.object,
                payload: case.payload,
            })
        })
    }
}

fn config() -> Config {
    Config {
        max_runs: 16,
        ..Config::default()
    }
}

fn keyed(operation: &Successor) -> (&str, i64) {
    match operation {
        Successor::Keyed { key, value } => (&key.value, value.value),
        other => panic!("expected a keyed successor, got {other}"),
    }
}

#[test]
fn generation_order_relations_and_collapse() {
    let adapter = Fake::new();
    let plan = plan_cases(&adapter, 6);

    let labels: Vec<String> = plan.cases.iter().map(Case::label).collect();
    assert_eq!(
        labels,
        vec![
            "target=target-keyed(k,1) -> successor=succ-keyed(k,1) [object=same, payload=same]",
            "target=target-keyed(k,1) -> successor=succ-keyed(k,9) [object=same, payload=different]",
            "target=target-keyed(k,1) -> successor=succ-keyed(other,1) [object=different, payload=same]",
            "target=target-keyed(k,1) -> successor=succ-keyed(other,9) [object=different, payload=different]",
            "target=target-keyed(k,1) -> successor=succ-keyless [object=n/a, payload=n/a]",
            "target=target-keyed(k,1) -> successor=succ-foreign(fk,99) [object=n/a, payload=n/a]",
        ]
    );
    assert_eq!(keyed(&plan.cases[0].successor), ("k", 1));
    assert_eq!(keyed(&plan.cases[1].successor), ("k", 9));
    assert_eq!(keyed(&plan.cases[2].successor), ("other", 1));
    assert_eq!(keyed(&plan.cases[3].successor), ("other", 9));

    // Applicable: 4 keyed x keyed + keyed x keyless + keyed x foreign
    // + keyless x keyed + keyless x keyless + keyless x foreign = 9.
    assert_eq!(plan.applicable(), 9);
    assert!(!plan.is_complete());
    assert_eq!(plan.unrun.len(), 3);
    assert!(plan.unrun.iter().any(|case| case.target == Target::Keyless));

    assert_eq!(plan.skipped.len(), 2);
    assert!(
        plan.skipped
            .iter()
            .all(|skip| skip.reason.contains("refuses"))
    );

    assert!(
        plan.notes
            .iter()
            .any(|note| note.contains("target kind test/node != successor kind test/other-node"))
    );
    assert!(
        plan.notes
            .iter()
            .any(|note| note.contains("target kind test/value != successor kind test/other-value"))
    );
    assert!(
        plan.notes
            .iter()
            .any(|note| note.contains("target has no business object; successor kind test/node"))
    );
}

#[test]
fn limit_does_not_count_unrun_cases_as_run() {
    let adapter = Fake::new();
    let runs = run_cases(&adapter, &config(), 4);

    assert_eq!(runs.results.len(), 4);
    assert_eq!(runs.plan.cases.len(), 4);
    assert_eq!(runs.plan.unrun.len(), 5);
    let run_labels: Vec<String> = runs.results.iter().map(|run| run.label()).collect();
    for unrun in &runs.plan.unrun {
        assert!(
            !run_labels.contains(&unrun.label()),
            "unrun case was counted as run"
        );
    }
    for result in &runs.results {
        assert!(
            result.report.baseline_errors.is_empty(),
            "{}",
            result.summary()
        );
        assert!(!result.report.trials.is_empty(), "{}", result.summary());
        assert!(
            result.report.calibration_errors.is_empty(),
            "{}",
            result.summary()
        );
    }

    // The successor runs in baselines and calibrations too, not just after a
    // cut. Four cases need at least a baseline plus one trial each; the exact
    // calibration count is the engine's business.
    let successor_runs = adapter.runs.load(Ordering::SeqCst);
    assert!(
        successor_runs > runs.results.len(),
        "successor ran only {successor_runs} times for {} cases",
        runs.results.len()
    );
}

#[test]
fn replay_keeps_case_description_and_plan() {
    let adapter = Fake::new();
    let runs = run_cases(&adapter, &config(), 1);
    let run = &runs.results[0];
    let trial = run
        .report
        .trials
        .first()
        .expect("fake adapter produced no plan");
    let plan = trial.replay_plan();
    let case = run.case.clone();
    let source_label = case.label();

    let replay =
        replay_case(&adapter, &config(), &case, &plan).expect("replay rejected a valid plan");
    assert_eq!(replay.label(), source_label);
    assert_eq!(replay.case, case);
    assert_eq!(replay.report.trials.len(), 1);
    let replay_trial = &replay.report.trials[0];
    assert!(replay_trial.is_realized(), "{}", replay.summary());
    assert_eq!(replay_trial.plan, plan.cuts);
    assert_eq!(replay.report.baseline_errors, Vec::<String>::new());

    let with_diagnostics = replay_case_with(&adapter, &config(), &case, &plan, Diagnostics::On)
        .expect("replay rejected a valid plan with diagnostics on");
    assert_eq!(with_diagnostics.summary(), replay.summary());
    assert_eq!(with_diagnostics.case, replay.case);
}

#[test]
fn different_uses_an_adapter_declared_representative_only_when_needed() {
    let adapter = Fake::new();
    let plan = plan_cases(&adapter, 8);
    // The foreign successor has different domains, so it keeps its seed.
    let foreign = plan
        .cases
        .iter()
        .find(|case| matches!(case.successor, Successor::Foreign { .. }))
        .expect("foreign successor case missing");
    let Successor::Foreign { key, value } = &foreign.successor else {
        unreachable!()
    };
    assert_eq!((key.value.as_str(), value.value), ("fk", 99));

    // The keyed successor's seed already differs from the target, so the same
    // seed is preserved rather than replaced by a representative.
    let different = plan
        .cases
        .iter()
        .find(|case| case.object == Some(ObjectRelation::Different))
        .expect("different-object case missing");
    assert_eq!(keyed(&different.successor).0, "other");
}

#[test]
fn invalid_representatives_and_ambiguous_slots_are_reported() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct One {
        key: Param<String>,
    }

    impl fmt::Display for One {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.key.value)
        }
    }

    impl ParamSlots for One {
        fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
            visitor.visit(SlotRole::Object, &mut self.key)
        }
    }

    struct OneAdapter {
        target: One,
        successor: One,
    }

    impl Adapter for OneAdapter {
        type Target = One;
        type Successor = One;
        type Obs = Obs;

        fn targets(&self) -> Vec<Self::Target> {
            vec![self.target.clone()]
        }

        fn successors(&self) -> Vec<Self::Successor> {
            vec![self.successor.clone()]
        }

        fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
            Expect::Atomic
        }

        fn scenario(
            &self,
            _ctx: Ctx,
            case: Case<Self::Target, Self::Successor>,
        ) -> CaseFuture<Self::Obs> {
            Box::pin(async move {
                Ok(Obs {
                    target: case.target.to_string(),
                    successor: case.successor.to_string(),
                    object: case.object,
                    payload: case.payload,
                })
            })
        }
    }

    // Same target and successor identity, no representative: Same succeeds,
    // Different must be a recorded capability error rather than a silent pass.
    let adapter = OneAdapter {
        target: One {
            key: Param::new("test/node", "k".to_string()),
        },
        successor: One {
            key: Param::new("test/node", "k".to_string()),
        },
    };
    let plan = plan_cases(&adapter, 8);
    assert_eq!(plan.cases.len(), 1);
    assert_eq!(plan.skipped.len(), 1);
    assert!(
        plan.skipped[0]
            .reason
            .contains("no declared representative different from 'k'"),
        "{}",
        plan.skipped[0].reason
    );

    // A representative set with duplicates is rejected at declaration time.
    let adapter = OneAdapter {
        target: One {
            key: Param::new("test/node", "k".to_string()),
        },
        successor: One {
            key: Param::new("test/node", "k".to_string())
                .with_alternatives(["other".to_string(), "other".to_string()]),
        },
    };
    let plan = plan_cases(&adapter, 8);
    assert!(plan.cases.is_empty());
    assert!(
        plan.skipped
            .iter()
            .any(|entry| entry.reason.contains("duplicate representative"))
    );

    // Two object slots in one operation are ambiguous, not "first wins".
    #[derive(Clone)]
    struct Ambiguous(One);
    impl ParamSlots for Ambiguous {
        fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
            visitor.visit(SlotRole::Object, &mut self.0.key)?;
            visitor.visit(SlotRole::Object, &mut self.0.key)
        }
    }
    impl fmt::Display for Ambiguous {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.0.fmt(f)
        }
    }
    struct AmbiguousAdapter {
        target: Ambiguous,
        successor: Ambiguous,
    }
    impl Adapter for AmbiguousAdapter {
        type Target = Ambiguous;
        type Successor = Ambiguous;
        type Obs = Obs;
        fn targets(&self) -> Vec<Self::Target> {
            vec![self.target.clone()]
        }
        fn successors(&self) -> Vec<Self::Successor> {
            vec![self.successor.clone()]
        }
        fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
            Expect::Atomic
        }
        fn scenario(
            &self,
            _ctx: Ctx,
            case: Case<Self::Target, Self::Successor>,
        ) -> CaseFuture<Self::Obs> {
            Box::pin(async move {
                Ok(Obs {
                    target: case.target.to_string(),
                    successor: case.successor.to_string(),
                    object: case.object,
                    payload: case.payload,
                })
            })
        }
    }
    let adapter = AmbiguousAdapter {
        target: Ambiguous(One {
            key: Param::new("test/node", "k".to_string()),
        }),
        successor: Ambiguous(One {
            key: Param::new("test/node", "other".to_string()),
        }),
    };
    let plan = plan_cases(&adapter, 8);
    assert!(plan.cases.is_empty());
    assert!(
        plan.skipped
            .iter()
            .any(|entry| entry.reason.contains("ambiguous object slot"))
    );
}

#[test]
fn same_domain_with_incompatible_rust_types_is_a_declaration_error() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct IntTarget {
        id: Param<i64>,
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct StringSuccessor {
        id: Param<String>,
    }
    impl fmt::Display for IntTarget {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("int-target")
        }
    }
    impl fmt::Display for StringSuccessor {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("string-successor")
        }
    }
    impl ParamSlots for IntTarget {
        fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
            visitor.visit(SlotRole::Object, &mut self.id)
        }
    }
    impl ParamSlots for StringSuccessor {
        fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
            visitor.visit(SlotRole::Object, &mut self.id)
        }
    }

    struct TypeConflict;
    impl Adapter for TypeConflict {
        type Target = IntTarget;
        type Successor = StringSuccessor;
        type Obs = Obs;
        fn targets(&self) -> Vec<Self::Target> {
            vec![IntTarget {
                id: Param::new("test/id", 1),
            }]
        }
        fn successors(&self) -> Vec<Self::Successor> {
            vec![StringSuccessor {
                id: Param::new("test/id", "one".to_string()),
            }]
        }
        fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
            Expect::Atomic
        }
        fn scenario(
            &self,
            _ctx: Ctx,
            case: Case<Self::Target, Self::Successor>,
        ) -> CaseFuture<Self::Obs> {
            Box::pin(async move {
                Ok(Obs {
                    target: case.target.to_string(),
                    successor: case.successor.to_string(),
                    object: case.object,
                    payload: case.payload,
                })
            })
        }
    }

    let plan = plan_cases(&TypeConflict, 8);
    assert!(plan.cases.is_empty());
    assert_eq!(plan.skipped.len(), 1);
    assert!(plan.skipped[0].reason.contains("incompatible Rust types"));
}

#[test]
fn no_registered_operations_is_reported_not_run() {
    struct Empty(Fake);

    impl Adapter for Empty {
        type Target = Target;
        type Successor = Successor;
        type Obs = Obs;

        fn targets(&self) -> Vec<Self::Target> {
            Vec::new()
        }

        fn successors(&self) -> Vec<Self::Successor> {
            self.0.successors()
        }

        fn expect(&self, case: &Case<Self::Target, Self::Successor>) -> Expect {
            self.0.expect(case)
        }

        fn scenario(
            &self,
            ctx: Ctx,
            case: Case<Self::Target, Self::Successor>,
        ) -> CaseFuture<Self::Obs> {
            self.0.scenario(ctx, case)
        }
    }

    let runs = run_cases(&Empty(Fake::new()), &config(), 8);
    assert!(runs.results.is_empty());
    assert!(runs.plan.skipped.is_empty());
    assert!(runs.plan.unrun.is_empty());
    assert!(
        runs.plan
            .notes
            .iter()
            .any(|note| note.contains("no cancellation target"))
    );
}

#[test]
fn diagnostics_do_not_change_unknown_or_coverage() {
    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
    struct UnstableObs {
        run: usize,
    }

    struct Unstable {
        runs: Arc<AtomicUsize>,
    }

    impl Adapter for Unstable {
        type Target = Target;
        type Successor = Successor;
        type Obs = UnstableObs;

        fn targets(&self) -> Vec<Self::Target> {
            vec![Target::Keyless]
        }

        fn successors(&self) -> Vec<Self::Successor> {
            vec![Successor::Keyless]
        }

        fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
            Expect::Atomic
        }

        fn scenario(
            &self,
            ctx: Ctx,
            _case: Case<Self::Target, Self::Successor>,
        ) -> CaseFuture<Self::Obs> {
            let runs = Arc::clone(&self.runs);
            Box::pin(async move {
                let run = runs.fetch_add(1, Ordering::SeqCst);
                let _ = ctx
                    .target(async {
                        tokio::task::yield_now().await;
                        0i64
                    })
                    .await;
                Ok(UnstableObs { run })
            })
        }
    }

    let adapter = Unstable {
        runs: Arc::new(AtomicUsize::new(0)),
    };
    let off = run_cases(&adapter, &config(), 8);
    let on = run_cases_with(&adapter, &config(), 8, Diagnostics::On);

    assert_eq!(off.plan.cases.len(), on.plan.cases.len());
    assert!(off.results.len() == on.results.len() && !off.results.is_empty());
    for (off, on) in off.results.iter().zip(&on.results) {
        assert_eq!(off.label(), on.label());
        assert_eq!(
            off.summary(),
            on.summary(),
            "diagnostics changed the report"
        );
        assert_eq!(off.report.coverage_gaps(), on.report.coverage_gaps());
        assert_eq!(off.report.notes, on.report.notes);
        assert!(on.report.verdict_count(dropwise::Verdict::Unknown) > 0);
        assert!(
            on.diagnostic_paths().contains(&"$.run".to_string()),
            "{:?}",
            on.diagnostic_paths()
        );
    }
}

#[test]
fn serialization_failure_is_reported_without_changing_the_verdict() {
    #[derive(Clone, Debug)]
    struct FailingObs;

    impl PartialEq for FailingObs {
        fn eq(&self, _other: &Self) -> bool {
            true
        }
    }

    impl serde::Serialize for FailingObs {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom(
                "observation serializer refuses this value",
            ))
        }
    }

    struct Failing;
    impl Adapter for Failing {
        type Target = Target;
        type Successor = Successor;
        type Obs = FailingObs;

        fn targets(&self) -> Vec<Self::Target> {
            vec![Target::Keyless]
        }

        fn successors(&self) -> Vec<Self::Successor> {
            vec![Successor::Keyless]
        }

        fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
            Expect::Atomic
        }

        fn scenario(
            &self,
            ctx: Ctx,
            _case: Case<Self::Target, Self::Successor>,
        ) -> CaseFuture<Self::Obs> {
            Box::pin(async move {
                let _ = ctx.target(async { 0i64 }).await;
                Ok(FailingObs)
            })
        }
    }

    let adapter = Failing;
    let off = run_cases(&adapter, &config(), 1);
    let on = run_cases_with(&adapter, &config(), 1, Diagnostics::On);
    assert_eq!(off.results[0].summary(), on.results[0].summary());
    assert!(
        on.results[0]
            .observation_diagnostics
            .errors
            .iter()
            .any(|error| error.contains("serialization failed")),
        "{:?}",
        on.results[0].observation_diagnostics
    );
    assert!(
        on.results[0].observation_diagnostics.comparisons.is_empty(),
        "a serialization failure must not be recorded as an empty comparison"
    );
}
