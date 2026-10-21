//! No-service checks for the shared target/successor combination generator.
//!
//! The adapter is intentionally fake: it records what the shared layer asked
//! for and returns the requested relation without touching a server. The tests
//! cover generation order, dimension collapse, applicability, the execution
//! limit, and replay using the same case description plus the engine's plan.

#[path = "../../../support/combine.rs"]
mod combine;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use combine::{
    Adapter, Case, CaseFuture, ObjectRelation, PayloadRelation, Shape, Slot, plan_cases,
    replay_case, run_cases,
};
use dropwise::{Config, Ctx, Expect};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Keyed { key: String, value: i64 },
    Keyless,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Keyed { key, value } => write!(f, "target-keyed({key},{value})"),
            Target::Keyless => write!(f, "target-keyless"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Successor {
    Keyed { key: String, value: i64 },
    Keyless,
    Foreign { key: String, value: i64 },
    Refused,
}

impl fmt::Display for Successor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Successor::Keyed { key, value } => write!(f, "succ-keyed({key},{value})"),
            Successor::Keyless => write!(f, "succ-keyless"),
            Successor::Foreign { key, value } => write!(f, "succ-foreign({key},{value})"),
            Successor::Refused => write!(f, "succ-refused"),
        }
    }
}

/// The observation intentionally contains only the realized case parameters.
/// It does not contain the target's completion/cancellation control result.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Self { runs: Arc::new(AtomicUsize::new(0)) }
    }

    fn shape_for_keyed(key: &str, kind: &'static str, value: i64, value_kind: &'static str) -> Shape {
        Shape {
            object: Some(Slot::new(kind, key)),
            payload: Some(Slot::new(value_kind, value.to_string())),
        }
    }
}

impl Adapter for Fake {
    type Target = Target;
    type Successor = Successor;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![Target::Keyed { key: "k".into(), value: 1 }, Target::Keyless]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            Successor::Keyed { key: "other".into(), value: 9 },
            Successor::Keyless,
            Successor::Foreign { key: "fk".into(), value: 99 },
            Successor::Refused,
        ]
    }

    fn target_shape(&self, target: &Self::Target) -> Shape {
        match target {
            Target::Keyed { key, value } => {
                Fake::shape_for_keyed(key, "test/node", *value, "test/value")
            }
            Target::Keyless => Shape::default(),
        }
    }

    fn successor_shape(&self, successor: &Self::Successor) -> Shape {
        match successor {
            Successor::Keyed { key, value } => {
                Fake::shape_for_keyed(key, "test/node", *value, "test/value")
            }
            Successor::Keyless | Successor::Refused => Shape::default(),
            Successor::Foreign { key, value } => {
                Fake::shape_for_keyed(key, "test/other-node", *value, "test/other-value")
            }
        }
    }

    fn realize(
        &self,
        target: &Self::Target,
        successor: &Self::Successor,
        object: Option<ObjectRelation>,
        payload: Option<PayloadRelation>,
    ) -> Result<Self::Successor, String> {
        let (target_key, target_value): (Option<&str>, Option<i64>) = match target {
            Target::Keyed { key, value } => (Some(key.as_str()), Some(*value)),
            Target::Keyless => (None, None),
        };
        match successor {
            Successor::Keyed { key, value } | Successor::Foreign { key, value } => {
                let object = match object {
                    Some(ObjectRelation::Same) => {
                        target_key.ok_or("target has no object to copy")?.to_string()
                    }
                    Some(ObjectRelation::Different) => {
                        if target_key == Some(key.as_str()) { "other".to_string() } else { key.clone() }
                    }
                    None => key.clone(),
                };
                let value = match payload {
                    Some(PayloadRelation::Same) => {
                        target_value.ok_or("target has no payload to copy")?
                    }
                    Some(PayloadRelation::Different) => *value,
                    None => *value,
                };
                match successor {
                    Successor::Keyed { .. } => Ok(Successor::Keyed { key: object, value }),
                    _ => Ok(Successor::Foreign { key: object, value }),
                }
            }
            Successor::Keyless | Successor::Refused => {
                if object.is_some() || payload.is_some() {
                    Err("keyless successor cannot realize a relation".into())
                } else {
                    Ok(successor.clone())
                }
            }
        }
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

    fn scenario(&self, ctx: Ctx, case: Case<Self::Target, Self::Successor>) -> CaseFuture<Self::Obs> {
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
    Config { max_runs: 16, ..Config::default() }
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
    assert_eq!(plan.cases[0].successor, Successor::Keyed { key: "k".into(), value: 1 });
    assert_eq!(plan.cases[1].successor, Successor::Keyed { key: "k".into(), value: 9 });
    assert_eq!(plan.cases[2].successor, Successor::Keyed { key: "other".into(), value: 1 });
    assert_eq!(plan.cases[3].successor, Successor::Keyed { key: "other".into(), value: 9 });

    // Applicable: 4 keyed x keyed + keyed x keyless + keyed x foreign
    // + keyless x keyed + keyless x keyless + keyless x foreign = 9.
    assert_eq!(plan.applicable(), 9);
    assert!(!plan.is_complete());
    assert_eq!(plan.unrun.len(), 3);
    assert!(plan.unrun.iter().any(|case| case.target == Target::Keyless));

    assert_eq!(plan.skipped.len(), 2);
    assert!(plan.skipped.iter().all(|skip| skip.reason.contains("refuses")));

    assert!(plan.notes.iter().any(|note| note.contains("target kind test/node != successor kind test/other-node")));
    assert!(plan.notes.iter().any(|note| note.contains("target kind test/value != successor kind test/other-value")));
    assert!(plan.notes.iter().any(|note| note.contains("target has no business object; successor kind test/node")));
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
        assert!(!run_labels.contains(&unrun.label()), "unrun case was counted as run");
    }
    for result in &runs.results {
        assert!(result.report.baseline_errors.is_empty(), "{}", result.summary());
        assert!(!result.report.trials.is_empty(), "{}", result.summary());
        assert!(result.report.calibration_errors.is_empty(), "{}", result.summary());
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
    let trial = run.report.trials.first().expect("fake adapter produced no plan");
    let plan = trial.replay_plan();
    let case = run.case.clone();
    let source_label = case.label();

    let replay = replay_case(&adapter, &config(), &case, &plan).expect("replay rejected a valid plan");
    assert_eq!(replay.label(), source_label);
    assert_eq!(replay.case, case);
    assert_eq!(replay.report.trials.len(), 1);
    let replay_trial = &replay.report.trials[0];
    assert!(replay_trial.is_realized(), "{}", replay.summary());
    assert_eq!(replay_trial.plan, plan.cuts);
    assert_eq!(replay.report.baseline_errors, Vec::<String>::new());
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

        fn target_shape(&self, target: &Self::Target) -> Shape {
            self.0.target_shape(target)
        }

        fn successor_shape(&self, successor: &Self::Successor) -> Shape {
            self.0.successor_shape(successor)
        }

        fn realize(
            &self,
            target: &Self::Target,
            successor: &Self::Successor,
            object: Option<ObjectRelation>,
            payload: Option<PayloadRelation>,
        ) -> Result<Self::Successor, String> {
            self.0.realize(target, successor, object, payload)
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
    assert!(runs.plan.notes.iter().any(|note| note.contains("no cancellation target")));
}
