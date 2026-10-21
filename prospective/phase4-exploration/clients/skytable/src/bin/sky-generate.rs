//! Generated target x successor cases for skytable (combine.rs).
//!
//! Target: one BlueQL `update` of a seeded row (value 1) on key `k`.
//! Successors: update / update+read-back / read, on the same or the alternate
//! key, with the same or a different payload. Pooled reuse only; the direct
//! mode is covered by the hand-written ladder.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use dropwise::{Ctx, Expect};
use p4e_skytable::{Env, Variant, bounded};

#[path = "../../../../../support/combine.rs"]
mod combine;

use combine::{
    Adapter, Case, CaseFuture, Param, ParamSlots, SlotRole, SlotVisitor, run_cases,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn run_id() -> usize {
    NEXT.fetch_add(1, SeqCst)
}

const TARGET_KEY: &str = "k";
const ALT_KEY: &str = "alt";
const TARGET_V: u64 = 1;
const SURVIVOR_V: u64 = 7;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetOp {
    Update { key: Param<String>, value: Param<u64> },
}

impl fmt::Display for TargetOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetOp::Update { key, value } => {
                write!(f, "update({},{})", key.value, value.value)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SuccessorOp {
    Update { key: Param<String>, value: Param<u64> },
    UpdateRead { key: Param<String>, value: Param<u64> },
    Select { key: Param<String> },
}

impl SuccessorOp {
    fn key(&self) -> &str {
        match self {
            SuccessorOp::Update { key, .. }
            | SuccessorOp::UpdateRead { key, .. }
            | SuccessorOp::Select { key } => &key.value,
        }
    }
}

impl fmt::Display for SuccessorOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuccessorOp::Update { key, value } => {
                write!(f, "update({},{})", key.value, value.value)
            }
            SuccessorOp::UpdateRead { key, value } => {
                write!(f, "update+read({},{})", key.value, value.value)
            }
            SuccessorOp::Select { key } => write!(f, "select({})", key.value),
        }
    }
}

impl ParamSlots for TargetOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            TargetOp::Update { key, value } => {
                visitor.visit(SlotRole::Object, key)?;
                visitor.visit(SlotRole::Payload, value)
            }
        }
    }
}

impl ParamSlots for SuccessorOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            SuccessorOp::Update { key, value } | SuccessorOp::UpdateRead { key, value } => {
                visitor.visit(SlotRole::Object, key)?;
                visitor.visit(SlotRole::Payload, value)
            }
            SuccessorOp::Select { key } => visitor.visit(SlotRole::Object, key),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct Obs {
    /// Observer read of the target's object after the cut, before the survivor.
    target_before: Result<u64, String>,
    write: Option<Result<(), String>>,
    own: Option<Result<u64, String>>,
    /// Observer read of the successor's object after the survivor.
    visible: Result<u64, String>,
}

fn key_param(value: &str) -> Param<String> {
    Param::new("sky/key", value.to_string())
}

fn successor_key_param() -> Param<String> {
    Param::new("sky/key", TARGET_KEY.to_string())
        .with_alternatives([ALT_KEY.to_string()])
}

fn value_param(value: u64) -> Param<u64> {
    Param::new("sky/value", value)
}

fn successor_value_param() -> Param<u64> {
    value_param(SURVIVOR_V).with_alternatives([TARGET_V])
}

struct SkyAdapter;

impl Adapter for SkyAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![TargetOp::Update {
            key: key_param(TARGET_KEY),
            value: value_param(TARGET_V),
        }]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            SuccessorOp::Update {
                key: successor_key_param(),
                value: successor_value_param(),
            },
            SuccessorOp::UpdateRead {
                key: successor_key_param(),
                value: successor_value_param(),
            },
            SuccessorOp::Select { key: successor_key_param() },
        ]
    }

    fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
        Expect::Atomic
    }

    fn scenario(&self, ctx: Ctx, case: Case<Self::Target, Self::Successor>) -> CaseFuture<Self::Obs> {
        Box::pin(async move {
            let mut env =
                Env::setup_keys(Variant::Pooled, run_id(), &[TARGET_KEY, ALT_KEY]).await?;
            let (target_key, target_value) = match &case.target {
                TargetOp::Update { key, value } => (key.value.clone(), value.value),
            };
            let completed = ctx.target(env.update_key(&target_key, target_value)).await;
            if let Some(Err(err)) = completed {
                let _ = bounded(p4e_skytable::BOUND, "cleanup", env.cleanup()).await;
                return Err(format!("completed target failed: {err}"));
            }
            let target_before =
                bounded(p4e_skytable::BOUND, "observer before survivor", env.observer_read_key(&target_key))
                    .await;
            let successor_key = case.successor.key().to_string();
            let (write, own) = match &case.successor {
                SuccessorOp::Update { key, value } => {
                    let write = bounded(
                        p4e_skytable::BOUND,
                        "successor update",
                        env.update_key(&key.value, value.value),
                    )
                    .await;
                    (Some(write), None)
                }
                SuccessorOp::UpdateRead { key, value } => {
                    let write = bounded(
                        p4e_skytable::BOUND,
                        "successor update",
                        env.update_key(&key.value, value.value),
                    )
                    .await;
                    let own = bounded(
                        p4e_skytable::BOUND,
                        "successor read-back",
                        env.select_key(&key.value),
                    )
                    .await;
                    (Some(write), Some(own))
                }
                SuccessorOp::Select { key } => {
                    let own = bounded(
                        p4e_skytable::BOUND,
                        "successor select",
                        env.select_key(&key.value),
                    )
                    .await;
                    (None, Some(own))
                }
            };
            let visible = bounded(
                p4e_skytable::BOUND,
                "observer after survivor",
                env.observer_read_key(&successor_key),
            )
            .await;
            let _ = bounded(p4e_skytable::BOUND, "cleanup", env.cleanup()).await;
            Ok(Obs { target_before, write, own, visible })
        })
    }
}

fn main() {
    let rep = std::env::args().nth(1).unwrap_or_else(|| "0".into());
    let runs = run_cases(&SkyAdapter, &p4e_skytable::config(), 10);
    runs.print();
    let mut violations = 0usize;
    for run in &runs.results {
        violations += run.report.violations().count();
        for trial in run.report.violations() {
            println!(
                "CASE-VIOLATION plan={} verdict={:?} detail={:?}",
                trial.plan_label(),
                trial.verdict,
                trial.invariant
            );
        }
    }
    println!(
        "CASE-END rep={rep} cases={} violations={violations}",
        runs.results.len()
    );
}
