//! Generated cancellation-target x successor cases for mysql_async's directly
//! held `Conn` (MY2), using the shared combination layer.
//!
//! The registered target is the existing known-value query (`SELECT 12345`).
//! It has no business object key, so the shared layer does not fabricate an
//! object relation; it expands the query-value relation instead and reports
//! the collapsed object dimension. Registered successors are a read-only query,
//! a query followed by a write and independent count, and a write alone.
//!
//! The target future only borrows the `Conn`; the handle stays in the scenario.
//! By default the successor uses the same `Conn` after cancellation. With
//! `DW_MYSQL_GUARD=1` the existing explicit usage contrast runs: check
//! `Conn::is_disconnected()` and replace the handle. That flag is a separate
//! recovery configuration and is printed as such; it is not silently inserted
//! and its effect is not attributed to a library patch alone.

#[path = "../../../support/combine.rs"]
mod combine;
#[path = "support/mysql_primitives.rs"]
mod primitives;

use std::fmt;
use std::time::Duration;

use combine::{
    Adapter, Case, CaseFuture, ObjectRelation, PayloadRelation, Shape, Slot, plan_cases,
    replay_case, run_case, run_cases,
};
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{Config, Ctx, Expect, Flavor, Race, Settle};
use mysql_async::prelude::Queryable;
use mysql_async::Conn;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetOp {
    Select { value: i64 },
}

impl fmt::Display for TargetOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetOp::Select { value } => write!(f, "select({value})"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SuccessorOp {
    Query { value: i64 },
    QueryThenInsert { value: i64, row: i32 },
    Insert { row: i32 },
}

impl fmt::Display for SuccessorOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuccessorOp::Query { value } => write!(f, "query({value})"),
            SuccessorOp::QueryThenInsert { value, row } => {
                write!(f, "query+insert({value},row={row})")
            }
            SuccessorOp::Insert { row } => write!(f, "insert(row={row})"),
        }
    }
}

/// Concrete successor returns and independent effects. The target's
/// completed/cancelled control result is deliberately not part of this value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// The successor's own read, when it performs one.
    own: Option<Result<i64, String>>,
    /// The successor's write result, when it performs one.
    write: Option<Result<(), String>>,
    /// Rows the successor's write made visible to the independent connection.
    visible: Option<Result<i64, String>>,
}

struct Env {
    conn: Option<Conn>,
    observer: Conn,
    table: String,
    direct_url: String,
    _proxy: Proxy,
}

impl Env {
    async fn new(url: &str) -> Result<Self, String> {
        let proxy = Proxy::start_with(
            primitives::upstream(url)?,
            ProxyOptions { reply_delay: primitives::REPLY_DELAY },
        )
        .await
        .map_err(primitives::e)?;
        let pooled = primitives::through(url, proxy.addr())?;
        let mut observer = primitives::connect(url).await?;
        let table = primitives::table_name();
        primitives::create_table(&mut observer, &table).await?;
        let mut conn = primitives::connect(&pooled).await?;
        conn.query_drop("SELECT 1").await.map_err(primitives::e)?;
        Ok(Self {
            conn: Some(conn),
            observer,
            table,
            direct_url: url.to_string(),
            _proxy: proxy,
        })
    }

    async fn recover(&mut self) -> Result<bool, String> {
        primitives::recover_direct(&mut self.conn, &self.direct_url).await
    }

    async fn successor(&mut self, op: &SuccessorOp) -> Obs {
        match op {
            SuccessorOp::Query { value } => {
                let conn = self.conn.as_mut().expect("direct Conn scenario");
                Obs { own: Some(primitives::select_value(conn, *value).await), write: None, visible: None }
            }
            SuccessorOp::QueryThenInsert { value, row } => {
                let table = self.table.clone();
                let conn = self.conn.as_mut().expect("direct Conn scenario");
                let own = primitives::select_value(conn, *value).await;
                let write = primitives::insert_row(conn, &table, *row).await;
                let visible = primitives::count_rows(&mut self.observer, &table).await;
                Obs { own: Some(own), write: Some(write), visible: Some(visible) }
            }
            SuccessorOp::Insert { row } => {
                let table = self.table.clone();
                let conn = self.conn.as_mut().expect("direct Conn scenario");
                let write = primitives::insert_row(conn, &table, *row).await;
                let visible = primitives::count_rows(&mut self.observer, &table).await;
                Obs { own: None, write: Some(write), visible: Some(visible) }
            }
        }
    }

    async fn cleanup(mut self) {
        self.conn.take();
        let _ = tokio::time::timeout(
            primitives::BOUND,
            primitives::drop_table(&mut self.observer, &self.table),
        )
        .await;
    }
}

struct MyAdapter {
    direct_url: String,
    guard: bool,
}

impl MyAdapter {
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            direct_url: primitives::url()?,
            guard: std::env::var("DW_MYSQL_GUARD").as_deref() == Ok("1"),
        })
    }
}

fn payload(value: i64) -> Slot {
    Slot::new("mysql/query-value", value.to_string())
}

fn target_value(target: &TargetOp) -> i64 {
    match target {
        TargetOp::Select { value } => *value,
    }
}

impl Adapter for MyAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![TargetOp::Select { value: 12345 }]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            SuccessorOp::Query { value: 67890 },
            SuccessorOp::QueryThenInsert { value: 67890, row: 1 },
            SuccessorOp::Insert { row: 2 },
        ]
    }

    fn target_shape(&self, target: &Self::Target) -> Shape {
        Shape { object: None, payload: Some(payload(target_value(target))) }
    }

    fn successor_shape(&self, successor: &Self::Successor) -> Shape {
        match successor {
            SuccessorOp::Query { value } | SuccessorOp::QueryThenInsert { value, .. } => {
                Shape { object: None, payload: Some(payload(*value)) }
            }
            SuccessorOp::Insert { row } => Shape {
                object: None,
                payload: Some(Slot::new("mysql/row-value", row.to_string())),
            },
        }
    }

    fn realize(
        &self,
        target: &Self::Target,
        successor: &Self::Successor,
        object: Option<ObjectRelation>,
        relation: Option<PayloadRelation>,
    ) -> Result<Self::Successor, String> {
        if object.is_some() {
            return Err("this target has no business object".into());
        }
        let target_value = target_value(target);
        match successor {
            SuccessorOp::Query { value } => Ok(SuccessorOp::Query {
                value: match relation {
                    Some(PayloadRelation::Same) => target_value,
                    Some(PayloadRelation::Different) => *value,
                    None => *value,
                },
            }),
            SuccessorOp::QueryThenInsert { value, row } => Ok(SuccessorOp::QueryThenInsert {
                value: match relation {
                    Some(PayloadRelation::Same) => target_value,
                    Some(PayloadRelation::Different) => *value,
                    None => *value,
                },
                row: *row,
            }),
            SuccessorOp::Insert { row } => match relation {
                Some(_) => Err("the insert row value is not comparable to the query value".into()),
                None => Ok(SuccessorOp::Insert { row: *row }),
            },
        }
    }

    fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
        Expect::Atomic
    }

    fn scenario(&self, ctx: Ctx, case: Case<Self::Target, Self::Successor>) -> CaseFuture<Self::Obs> {
        let url = self.direct_url.clone();
        let guard = self.guard;
        Box::pin(async move {
            let mut env = Env::new(&url).await?;
            let value = target_value(&case.target);
            let completed = {
                let conn = env.conn.as_mut().expect("direct Conn scenario");
                ctx.target(primitives::select_value(conn, value)).await
            };
            if let Some(Err(err)) = completed {
                env.cleanup().await;
                return Err(format!("completed target failed: {err}"));
            }
            if guard {
                if let Err(err) = env.recover().await {
                    env.cleanup().await;
                    return Err(err);
                }
            }
            let obs = env.successor(&case.successor).await;
            env.cleanup().await;
            Ok(obs)
        })
    }
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 32,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

#[test]
fn generated_my2_cases() {
    let adapter = MyAdapter::from_env()
        .unwrap_or_else(|err| panic!("mysql generated cases need the phase 3 environment: {err}"));
    println!(
        "MYSQL-GEN guard={} reply_delay={:?} url={}",
        adapter.guard,
        primitives::REPLY_DELAY,
        adapter.direct_url
    );
    let runs = run_cases(&adapter, &config(), 8);
    runs.print();

    // The registered target has no object key; the two query successors should
    // produce same/different value cases, and the insert's row value is not
    // comparable to the query value, so it must not fabricate a payload pair.
    assert_eq!(runs.plan.cases.len(), 5, "{}", runs.plan.notes.join(" | "));
    assert!(runs.plan.skipped.is_empty());
    assert!(runs.plan.unrun.is_empty());
    assert_eq!(
        runs.plan
            .cases
            .iter()
            .filter(|case| case.payload == Some(PayloadRelation::Different))
            .count(),
        2
    );
    assert!(runs.plan.notes.iter().any(|note| {
        note.contains("target kind mysql/query-value != successor kind mysql/row-value")
    }));
}

#[test]
fn replay_generated_my2_case() {
    let adapter = MyAdapter::from_env()
        .unwrap_or_else(|err| panic!("mysql replay needs the phase 3 environment: {err}"));
    let config = config();
    let plan = plan_cases(&adapter, 8);
    let case = plan
        .cases
        .iter()
        .find(|case| {
            case.successor == SuccessorOp::Query { value: 67890 }
                && case.payload == Some(PayloadRelation::Different)
                && case.object.is_none()
        })
        .expect("the different-value query case was not generated")
        .clone();
    let run = run_case(&adapter, &config, &case);
    println!("MYSQL-REPLAY source={}", run.summary());
    let source = run
        .report
        .trials
        .first()
        .expect("generated case produced no cancellation plan");
    let replay_plan = source.replay_plan();
    println!(
        "MYSQL-REPLAY source_plan={} source_verdict={:?} source_detail={:?}",
        source.plan_label(),
        source.verdict,
        source.invariant
    );
    let replay = replay_case(&adapter, &config, &case, &replay_plan)
        .expect("replay rejected the generated case's plan");
    println!("MYSQL-REPLAY {}", replay.summary());
    let trial = replay.report.trials.first().expect("replay produced no trial");
    println!(
        "MYSQL-REPLAY plan={} realized={} verdict={:?} comparison={:?} invariant={:?}",
        trial.plan_label(),
        trial.is_realized(),
        trial.verdict,
        trial.comparison,
        trial.invariant
    );
    assert_eq!(replay.label(), run.label());
    assert_eq!(trial.plan, replay_plan.cuts);
}
