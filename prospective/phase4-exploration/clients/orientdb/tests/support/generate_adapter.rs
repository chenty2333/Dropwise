// Generated cancellation-target x successor cases for the pooled OrientDB client.
//
// Targets: `checkout` (keyless) and `update(primary, 1)` (checkout + complete
// SQL script on the pre-created record).
// Successors: `update(record, value)`, `update+read(record, value)` and
// `read(record)`. Same/different object and same/different payload are
// expanded where both sides declare the domain. Values and marker strings make
// the reply attribution observable; the independent observer reads the record.

use crate::combine::{
    Adapter, Case, CaseFuture, Diagnostics, Param, ParamSlots, PayloadRelation, SlotRole,
    SlotVisitor, plan_cases, run_cases_with,
};
use p4e_orientdb::*;

use std::fmt;

use dropwise::{Config, Ctx, Expect, Flavor, Race, Settle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordRole {
    Primary,
    Alternate,
}

impl RecordRole {
    fn as_str(self) -> &'static str {
        match self {
            RecordRole::Primary => "primary",
            RecordRole::Alternate => "alternate",
        }
    }
    fn other(self) -> Self {
        match self {
            RecordRole::Primary => RecordRole::Alternate,
            RecordRole::Alternate => RecordRole::Primary,
        }
    }
}

impl fmt::Display for RecordRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetOp {
    Checkout,
    Update { record: Param<RecordRole>, value: Param<i32> },
}

impl TargetOp {
    fn record(&self) -> Option<RecordRole> {
        match self {
            TargetOp::Checkout => None,
            TargetOp::Update { record, .. } => Some(record.value),
        }
    }
}

impl fmt::Display for TargetOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetOp::Checkout => f.write_str("checkout"),
            TargetOp::Update { record, value } => write!(f, "update({},{})", record.value, value.value),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SuccessorOp {
    Update { record: Param<RecordRole>, value: Param<i32> },
    UpdateRead { record: Param<RecordRole>, value: Param<i32> },
    Read { record: Param<RecordRole> },
}

impl SuccessorOp {
    fn record(&self) -> RecordRole {
        match self {
            SuccessorOp::Update { record, .. }
            | SuccessorOp::UpdateRead { record, .. }
            | SuccessorOp::Read { record } => record.value,
        }
    }
}

impl fmt::Display for SuccessorOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuccessorOp::Update { record, value } => write!(f, "update({},{})", record.value, value.value),
            SuccessorOp::UpdateRead { record, value } => {
                write!(f, "update+read({},{})", record.value, value.value)
            }
            SuccessorOp::Read { record } => write!(f, "read({})", record.value),
        }
    }
}

impl ParamSlots for TargetOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            TargetOp::Checkout => Ok(()),
            TargetOp::Update { record, value } => {
                visitor.visit(SlotRole::Object, record)?;
                visitor.visit(SlotRole::Payload, value)
            }
        }
    }
}

impl ParamSlots for SuccessorOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            SuccessorOp::Update { record, value } | SuccessorOp::UpdateRead { record, value } => {
                visitor.visit(SlotRole::Object, record)?;
                visitor.visit(SlotRole::Payload, value)
            }
            SuccessorOp::Read { record } => visitor.visit(SlotRole::Object, record),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct Obs {
    own_write: Option<Result<Reply, String>>,
    own_read: Option<Result<Reply, String>>,
    visible: Result<Reply, String>,
    pool: (u64, u64, u64, u64),
}

fn record_param(role: RecordRole) -> Param<RecordRole> {
    Param::new("orientdb/record", role)
}

fn successor_record_param(role: RecordRole) -> Param<RecordRole> {
    Param::new("orientdb/record", role).with_alternatives([role.other()])
}

fn value_param(value: i32) -> Param<i32> {
    Param::new("orientdb/write-value", value)
}

fn successor_value_param() -> Param<i32> {
    value_param(SURVIVOR_V).with_alternatives([TARGET_V])
}

struct OrientAdapter;

impl Adapter for OrientAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![
            TargetOp::Update {
                record: record_param(RecordRole::Primary),
                value: value_param(TARGET_V),
            },
            TargetOp::Checkout,
        ]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            SuccessorOp::Update {
                record: successor_record_param(RecordRole::Primary),
                value: successor_value_param(),
            },
            SuccessorOp::UpdateRead {
                record: successor_record_param(RecordRole::Primary),
                value: successor_value_param(),
            },
            SuccessorOp::Read {
                record: successor_record_param(RecordRole::Primary),
            },
        ]
    }

    fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
        Expect::Atomic
    }

    fn scenario(&self, ctx: Ctx, case: Case<Self::Target, Self::Successor>) -> CaseFuture<Self::Obs> {
        Box::pin(async move {
            let env = setup(&["primary", "alternate"]).await?;
            let target_done: Option<Result<Reply, String>> = match &case.target {
                TargetOp::Checkout => ctx
                    .target(env.pool.get())
                    .await
                    .map(|result| result.map(|_| (0, String::new())).map_err(e)),
                TargetOp::Update { record, value } => {
                    let record = env.record(record.value.as_str())?.to_string();
                    ctx.target(env.pooled_update(&record, value.value, "target"))
                        .await
                        .map(|result| result.map(|reply| (reply.0, reply.1)))
                }
            };
            if let Some(Err(err)) = target_done {
                let _ = bounded("cleanup", env.cleanup()).await;
                return Err(format!("completed target failed: {err}"));
            }
            let record = env.record(case.successor.record().as_str())?.to_string();
            let (own_write, own_read) = match &case.successor {
                SuccessorOp::Update { value, .. } => (
                    Some(bounded("successor update", env.pooled_update(&record, value.value, "successor")).await),
                    None,
                ),
                SuccessorOp::UpdateRead { value, .. } => (
                    Some(bounded("successor update", env.pooled_update(&record, value.value, "successor")).await),
                    Some(bounded("successor read", env.pooled_read(&record)).await),
                ),
                SuccessorOp::Read { .. } => (
                    None,
                    Some(bounded("successor read", env.pooled_read(&record)).await),
                ),
            };
            let visible = bounded("observer read", env.observer_read(&record)).await;
            let pool = env.pooled_stats().await;
            let _ = bounded("cleanup", env.cleanup()).await;
            Ok(Obs { own_write, own_read, visible, pool })
        })
    }
}

fn gen_config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_cancellations: 1,
        max_runs: 24,
        scenario_timeout: std::time::Duration::from_secs(20),
        settle: Settle { tokio_time: std::time::Duration::ZERO, real_time: std::time::Duration::ZERO, watchdog: std::time::Duration::from_secs(10) },
        ..Config::default()
    }
}

#[test]
fn orientdb_plan_without_service() {
    let adapter = OrientAdapter;
    let plan = plan_cases(&adapter, 16);
    let labels: Vec<String> = plan.cases.iter().map(Case::label).collect();
    for label in &labels {
        println!("GEN-PLAN {label}");
    }
    assert_eq!(plan.cases.len(), 13, "{}", plan.notes.join(" | "));
    assert!(plan.skipped.is_empty());
    assert!(plan.unrun.is_empty());
    let count_object = |relation| {
        plan.cases.iter().filter(|case| case.object == Some(relation)).count()
    };
    let count_payload = |relation| {
        plan.cases.iter().filter(|case| case.payload == Some(relation)).count()
    };
    assert_eq!(count_object(crate::combine::ObjectRelation::Same), 5);
    assert_eq!(count_object(crate::combine::ObjectRelation::Different), 5);
    assert_eq!(count_payload(PayloadRelation::Same), 4);
    assert_eq!(count_payload(PayloadRelation::Different), 4);
    assert!(
        plan.notes.iter().any(|note| note.contains("target has no business object")),
        "{}",
        plan.notes.join(" | ")
    );
}

#[test]
fn generated_orientdb_cases() {
    let cfg = ServerCfg::from_env().expect("generated cases need the round-3 environment");
    println!("GEN environment db={} port={} reply_delay={REPLY_DELAY:?}", cfg.db, cfg.port);
    let runs = run_cases_with(&OrientAdapter, &gen_config(), 16, Diagnostics::On);
    runs.print();
    assert_eq!(runs.plan.cases.len(), 13, "{}", runs.plan.notes.join(" | "));
    assert!(runs.plan.skipped.is_empty());
    assert!(runs.plan.unrun.is_empty());
}
