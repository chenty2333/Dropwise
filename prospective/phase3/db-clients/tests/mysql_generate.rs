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
    Adapter, Case, CaseFuture, Diagnostics, Param, ParamSlots, PayloadRelation, SlotRole,
    SlotVisitor, plan_cases, replay_case, run_case, run_cases_with,
};
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{Config, Ctx, Expect, Flavor, Race, Settle};
use mysql_async::Conn;
use mysql_async::prelude::Queryable;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetOp {
    Select { value: Param<i64> },
}

impl fmt::Display for TargetOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetOp::Select { value } => write!(f, "select({})", value.value),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SuccessorOp {
    Query { value: Param<i64> },
    QueryThenInsert { value: Param<i64>, row: Param<i32> },
    Insert { row: Param<i32> },
}

impl fmt::Display for SuccessorOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuccessorOp::Query { value } => write!(f, "query({})", value.value),
            SuccessorOp::QueryThenInsert { value, row } => {
                write!(f, "query+insert({},row={})", value.value, row.value)
            }
            SuccessorOp::Insert { row } => write!(f, "insert(row={})", row.value),
        }
    }
}

impl ParamSlots for TargetOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            TargetOp::Select { value } => visitor.visit(SlotRole::Payload, value),
        }
    }
}

impl ParamSlots for SuccessorOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            // `row` in QueryThenInsert is a fixed step, not a compared dimension.
            SuccessorOp::Query { value } | SuccessorOp::QueryThenInsert { value, .. } => {
                visitor.visit(SlotRole::Payload, value)
            }
            SuccessorOp::Insert { row } => visitor.visit(SlotRole::Payload, row),
        }
    }
}

/// Concrete successor returns and independent effects. The target's
/// completed/cancelled control result is deliberately not part of this value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
            ProxyOptions {
                reply_delay: primitives::REPLY_DELAY,
            },
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
                Obs {
                    own: Some(primitives::select_value(conn, value.value).await),
                    write: None,
                    visible: None,
                }
            }
            SuccessorOp::QueryThenInsert { value, row } => {
                let table = self.table.clone();
                let conn = self.conn.as_mut().expect("direct Conn scenario");
                let own = primitives::select_value(conn, value.value).await;
                let write = primitives::insert_row(conn, &table, row.value).await;
                let visible = primitives::count_rows(&mut self.observer, &table).await;
                Obs {
                    own: Some(own),
                    write: Some(write),
                    visible: Some(visible),
                }
            }
            SuccessorOp::Insert { row } => {
                let table = self.table.clone();
                let conn = self.conn.as_mut().expect("direct Conn scenario");
                let write = primitives::insert_row(conn, &table, row.value).await;
                let visible = primitives::count_rows(&mut self.observer, &table).await;
                Obs {
                    own: None,
                    write: Some(write),
                    visible: Some(visible),
                }
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

const TARGET_VALUE: i64 = 12345;
const QUERY_VALUE: i64 = 67890;
const INSERT_ROW: i32 = 1;
const OTHER_ROW: i32 = 2;

fn query_value(value: i64) -> Param<i64> {
    Param::new("mysql/query-value", value)
}

fn row_value(value: i32) -> Param<i32> {
    Param::new("mysql/row-value", value)
}

impl Adapter for MyAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![TargetOp::Select {
            value: query_value(TARGET_VALUE),
        }]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            SuccessorOp::Query {
                value: query_value(QUERY_VALUE).with_alternatives([TARGET_VALUE]),
            },
            SuccessorOp::QueryThenInsert {
                value: query_value(QUERY_VALUE).with_alternatives([TARGET_VALUE]),
                row: row_value(INSERT_ROW),
            },
            SuccessorOp::Insert {
                row: row_value(OTHER_ROW).with_alternatives([INSERT_ROW]),
            },
        ]
    }

    fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
        Expect::Atomic
    }

    fn scenario(
        &self,
        ctx: Ctx,
        case: Case<Self::Target, Self::Successor>,
    ) -> CaseFuture<Self::Obs> {
        let url = self.direct_url.clone();
        let guard = self.guard;
        Box::pin(async move {
            let mut env = Env::new(&url).await?;
            let TargetOp::Select { value } = &case.target;
            let completed = {
                let conn = env.conn.as_mut().expect("direct Conn scenario");
                ctx.target(primitives::select_value(conn, value.value))
                    .await
            };
            if let Some(Err(err)) = completed {
                env.cleanup().await;
                return Err(format!("completed target failed: {err}"));
            }
            if guard && let Err(err) = env.recover().await {
                env.cleanup().await;
                return Err(err);
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
        settle: Settle {
            tokio_time: Duration::ZERO,
            ..Settle::default()
        },
        ..Config::default()
    }
}

#[test]
fn mysql_plan_without_service() {
    let adapter = MyAdapter {
        direct_url: String::new(),
        guard: false,
    };
    let plan = plan_cases(&adapter, 8);
    let labels: Vec<String> = plan.cases.iter().map(Case::label).collect();
    assert_eq!(
        labels,
        vec![
            format!(
                "target=select({TARGET_VALUE}) -> successor=query({TARGET_VALUE}) [object=n/a, payload=same]"
            ),
            format!(
                "target=select({TARGET_VALUE}) -> successor=query({QUERY_VALUE}) [object=n/a, payload=different]"
            ),
            format!(
                "target=select({TARGET_VALUE}) -> successor=query+insert({TARGET_VALUE},row={INSERT_ROW}) [object=n/a, payload=same]"
            ),
            format!(
                "target=select({TARGET_VALUE}) -> successor=query+insert({QUERY_VALUE},row={INSERT_ROW}) [object=n/a, payload=different]"
            ),
            format!(
                "target=select({TARGET_VALUE}) -> successor=insert(row={OTHER_ROW}) [object=n/a, payload=n/a]"
            ),
        ]
    );
    assert_eq!(plan.cases.len(), 5);
    assert!(plan.skipped.is_empty());
    assert!(plan.unrun.is_empty());
    assert_eq!(
        plan.cases
            .iter()
            .filter(|case| case.payload == Some(PayloadRelation::Different))
            .count(),
        2
    );
    assert!(plan.notes.iter().any(|note| {
        note.contains("target kind mysql/query-value != successor kind mysql/row-value")
    }));
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
    let runs = run_cases_with(&adapter, &config(), 8, Diagnostics::On);
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
            matches!(
                &case.successor,
                SuccessorOp::Query { value } if value.value == QUERY_VALUE
            ) && case.payload == Some(PayloadRelation::Different)
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
    let trial = replay
        .report
        .trials
        .first()
        .expect("replay produced no trial");
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
